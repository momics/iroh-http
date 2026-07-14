//! Generic DNS-SD (RFC 6763) advertise/browse engine.
//!
//! This is the single mDNS engine used by the crate. It knows nothing about
//! iroh: you hand it a [`ServiceConfig`] (or [`BrowseConfig`]) and it publishes
//! or discovers plain DNS-SD services, surfacing the full [`ServiceRecord`]
//! (instance, host, port, addresses, and *all* TXT properties) without dropping
//! any field.
//!
//! The iroh-http–specific path ([`crate::advertise_peer`] /
//! [`crate::browse_peers`]) is a thin specialization layered on top of this
//! module: it builds a [`ServiceConfig`] from an [`iroh::Endpoint`] and wires
//! browse results into the endpoint's address lookup. See ADR-018.

use std::{
    future::Future,
    net::{IpAddr, SocketAddr, SocketAddrV6},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
};

use futures::FutureExt;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::{
    engine::{service_type, BoxFuture, BrowseHandle, RawEvent, TransportError},
    DiscoveryError,
};

pub use crate::engine::{BrowseConfig, Protocol, ServiceConfig, ServiceRecord};

const DAEMON_COMMAND_RETRIES: usize = 100;

fn retry_daemon_command<T>(mut command: impl FnMut() -> mdns_sd::Result<T>) -> mdns_sd::Result<T> {
    let mut retries = DAEMON_COMMAND_RETRIES;
    loop {
        match command() {
            Err(mdns_sd::Error::Again) if retries != 0 => {
                retries = retries.saturating_sub(1);
                thread::sleep(std::time::Duration::from_millis(1));
            }
            result => return result,
        }
    }
}

fn retire_after_enqueue<T, E>(
    current: &mut Option<ServiceConfig>,
    enqueue: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let response = enqueue()?;
    current.take();
    Ok(response)
}

// ── Service-type helpers ─────────────────────────────────────────────────────

/// Extract the service instance label from a DNS-SD fullname like
/// `<instance>._my-app._udp.local.`, given the known `ty_domain` suffix
/// (`_my-app._udp.local.`) reported alongside it.
///
/// Stripping the exact, known suffix — rather than splitting on the first
/// `"._"` — keeps this correct for the *generic* DNS-SD surface, where an
/// instance label can itself legally contain `._` (`mdns-sd` escapes literal
/// dots in TXT/instance data with a leading `\`, but an instance name built
/// from arbitrary bytes can still contain the two-character sequence `._`
/// without escaping). A naive first-match split would then chop the label
/// short.
pub(crate) fn instance_from_fullname(fullname: &str, ty_domain: &str) -> Option<String> {
    let suffix = format!(".{ty_domain}");
    let instance = fullname.strip_suffix(suffix.as_str())?;
    if instance.is_empty() {
        None
    } else {
        Some(instance.to_string())
    }
}

/// Create a fresh mDNS daemon for a single browse or advertise session.
///
/// Each session owns its own daemon (rather than sharing one process-wide) so
/// that its lifetime is tied to the session: dropping the session shuts the
/// daemon down. It also means an advertise and a browse in the *same* process
/// run on separate daemons and can discover each other over loopback multicast
/// — `mdns-sd` does not deliver a daemon's own registrations back to its own
/// browsers.
pub(crate) fn new_daemon() -> Result<ServiceDaemon, DiscoveryError> {
    ServiceDaemon::new().map_err(|e| DiscoveryError::Setup(e.to_string()))
}

// ── Record conversion ────────────────────────────────────────────────────────

fn socket_addr_with_scope(ip: IpAddr, port: u16, scope_id: u32) -> SocketAddr {
    match ip {
        IpAddr::V4(ip) => SocketAddr::new(IpAddr::V4(ip), port),
        IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(ip, port, 0, scope_id)),
    }
}

fn applicable_ipv6_scope(ip: &std::net::Ipv6Addr, discovered_scope: u32) -> u32 {
    if (ip.segments()[0] & 0xffc0) == 0xfe80 {
        discovered_scope
    } else {
        0
    }
}

fn socket_addr_from_scoped(ip: &mdns_sd::ScopedIp, port: u16) -> SocketAddr {
    let ip_addr = ip.to_ip_addr();
    let scope_id = match ip {
        mdns_sd::ScopedIp::V6(ip) => applicable_ipv6_scope(ip.addr(), ip.scope_id().index),
        _ => 0,
    };
    socket_addr_with_scope(ip_addr, port, scope_id)
}

fn record_from_resolved(rs: &mdns_sd::ResolvedService) -> Option<ServiceRecord> {
    let instance_name = instance_from_fullname(&rs.fullname, &rs.ty_domain)?;
    let addrs = rs
        .addresses
        .iter()
        .map(|scoped| socket_addr_from_scoped(scoped, rs.port))
        .collect();
    let txt = rs
        .txt_properties
        .iter()
        .map(|p| (p.key().to_string(), p.val_str().to_string()))
        .collect();
    Some(ServiceRecord {
        is_active: true,
        service_type: rs.ty_domain.clone(),
        instance_name,
        host: Some(rs.host.clone()),
        port: rs.port,
        addrs,
        txt,
    })
}

fn raw_event_from_removed(service_type: String, fullname: &str) -> Option<RawEvent> {
    let instance_name = instance_from_fullname(fullname, &service_type)?;
    Some(RawEvent::Remove {
        service_type,
        instance_name,
    })
}

// ── Advertise ────────────────────────────────────────────────────────────────

fn service_info(
    config: &ServiceConfig,
    enable_addr_auto: bool,
) -> Result<ServiceInfo, DiscoveryError> {
    let service_type = service_type(&config.service_name, config.protocol)?;
    let host_name = format!("{}.local.", config.instance_name);
    let info = ServiceInfo::new(
        &service_type,
        &config.instance_name,
        &host_name,
        &config.addrs[..],
        config.port,
        &config.txt[..],
    )
    .map_err(|error| DiscoveryError::Setup(error.to_string()))?;
    Ok(if enable_addr_auto {
        info.enable_addr_auto()
    } else {
        info
    })
}

fn register_changed(
    current: &mut Option<ServiceConfig>,
    next: ServiceConfig,
    enable_addr_auto: bool,
    register: impl FnOnce(ServiceInfo) -> Result<(), DiscoveryError>,
) -> Result<bool, DiscoveryError> {
    if current.as_ref() == Some(&next) {
        return Ok(false);
    }
    register(service_info(&next, enable_addr_auto)?)?;
    *current = Some(next);
    Ok(true)
}

#[derive(Clone)]
pub(crate) struct AdvertiseUpdater {
    daemon: ServiceDaemon,
    fullname: String,
    current: Arc<Mutex<Option<ServiceConfig>>>,
    enable_addr_auto: bool,
}

impl AdvertiseUpdater {
    pub(crate) fn update(&self, next: ServiceConfig) -> Result<bool, DiscoveryError> {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if current.is_none() {
            return Ok(false);
        }
        register_changed(&mut current, next, self.enable_addr_auto, |info| {
            if info.get_fullname() != self.fullname {
                return Err(DiscoveryError::Setup(
                    "cannot change an active DNS-SD advertisement identity".to_string(),
                ));
            }
            self.daemon
                .register(info)
                .map_err(|error| DiscoveryError::Setup(error.to_string()))
        })
    }

    pub(crate) fn unregister(&self) {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if current.is_none() {
            return;
        }
        match retire_after_enqueue(&mut current, || {
            retry_daemon_command(|| self.daemon.unregister(&self.fullname))
        }) {
            Ok(_) => {}
            Err(mdns_sd::Error::DaemonShutdown) => {
                // A stopped daemon cannot still advertise the record.
                current.take();
            }
            Err(error) => {
                tracing::warn!(%error, "iroh-http-discovery: could not enqueue DNS-SD unregister; retaining state for retry");
            }
        }
    }
}

/// An active advertise session.
///
/// Drop to stop advertising; this unregisters the DNS-SD service and shuts the
/// session's mDNS daemon down. A peer-specialized session can also own a refresh
/// worker; drop cancels and joins that worker before unregistering.
pub struct AdvertiseSession {
    daemon: ServiceDaemon,
    updater: AdvertiseUpdater,
    refresh_worker: Option<RefreshWorker>,
}

pub(crate) struct RefreshWorker {
    cancel: Option<futures::channel::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl RefreshWorker {
    fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(thread) = self.thread.take() {
            // The worker never owns its `AdvertiseSession`, but keep this guard
            // so future refactors cannot accidentally self-join.
            if thread.thread().id() != thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

pub(crate) fn spawn_refresh_until_closed<C, R, F>(
    closed: C,
    refresh: R,
    on_finish: F,
) -> Result<RefreshWorker, DiscoveryError>
where
    C: Future<Output = ()> + Send + 'static,
    R: Future<Output = ()> + Send + 'static,
    F: FnOnce() + Send + 'static,
{
    let (cancel, cancel_rx) = futures::channel::oneshot::channel();
    let thread = thread::Builder::new()
        .name("iroh-http-mdns-refresh".to_string())
        .spawn(move || {
            futures::executor::block_on(async move {
                let cancel = cancel_rx.fuse();
                let closed = closed.fuse();
                let refresh = refresh.fuse();
                futures::pin_mut!(cancel, closed, refresh);
                futures::select_biased! {
                    _ = cancel => {}
                    _ = closed => {}
                    _ = refresh => {}
                }
            });
            on_finish();
        })
        .map_err(|error| {
            DiscoveryError::Setup(format!("cannot start peer advertisement refresh: {error}"))
        })?;
    Ok(RefreshWorker {
        cancel: Some(cancel),
        thread: Some(thread),
    })
}

impl AdvertiseSession {
    pub(crate) fn updater(&self) -> AdvertiseUpdater {
        self.updater.clone()
    }

    pub(crate) fn set_refresh_worker(&mut self, worker: RefreshWorker) {
        debug_assert!(self.refresh_worker.is_none());
        self.refresh_worker = Some(worker);
    }
}

impl Drop for AdvertiseSession {
    fn drop(&mut self) {
        if let Some(mut worker) = self.refresh_worker.take() {
            worker.cancel();
        }
        self.updater.unregister();
        let _ = retry_daemon_command(|| self.daemon.shutdown());
    }
}

/// Advertise a service on the local network via DNS-SD.
///
/// Publishes `PTR` + `SRV` + `TXT` + `A`/`AAAA` records under
/// `_<service_name>.<proto>.local.`. Local interface addresses are added and
/// kept fresh automatically ([`ServiceInfo::enable_addr_auto`]); any
/// [`ServiceConfig::addrs`] are advertised in addition. The service stays
/// advertised until the returned [`AdvertiseSession`] is dropped.
pub fn advertise(config: ServiceConfig) -> Result<AdvertiseSession, DiscoveryError> {
    advertise_inner(config, true)
}

/// Advertise an explicitly selected address set, enabling `mdns-sd` host
/// interface expansion only when the caller has established that its single
/// SRV port is valid for every bound family.
///
/// Peer advertisements use this because their authoritative QUIC candidates
/// can carry different per-family or reflexive ports in TXT; unconditionally
/// pairing every local interface with one SRV port would manufacture
/// undialable paths.
pub(crate) fn advertise_selected_addrs(
    config: ServiceConfig,
    enable_addr_auto: bool,
) -> Result<AdvertiseSession, DiscoveryError> {
    advertise_inner(config, enable_addr_auto)
}

fn advertise_inner(
    config: ServiceConfig,
    enable_addr_auto: bool,
) -> Result<AdvertiseSession, DiscoveryError> {
    let info = service_info(&config, enable_addr_auto)?;
    let daemon = new_daemon()?;

    let fullname = info.get_fullname().to_string();
    if let Err(error) = daemon.register(info) {
        let _ = retry_daemon_command(|| daemon.shutdown());
        return Err(DiscoveryError::Setup(error.to_string()));
    }

    let updater = AdvertiseUpdater {
        daemon: daemon.clone(),
        fullname: fullname.clone(),
        current: Arc::new(Mutex::new(Some(config))),
        enable_addr_auto,
    };
    Ok(AdvertiseSession {
        daemon,
        updater,
        refresh_worker: None,
    })
}

// ── Browse ───────────────────────────────────────────────────────────────────

struct MdnsBrowse {
    receiver: mdns_sd::Receiver<ServiceEvent>,
    daemon: ServiceDaemon,
    service_type: String,
    closed: AtomicBool,
}

impl BrowseHandle for MdnsBrowse {
    fn next(&self) -> BoxFuture<'_, Result<Option<RawEvent>, TransportError>> {
        Box::pin(async move {
            if self.closed.load(Ordering::Acquire) {
                return Ok(None);
            }
            loop {
                let event = match self.receiver.recv_async().await {
                    Ok(event) => event,
                    Err(_) => return Ok(None),
                };
                if self.closed.load(Ordering::Acquire) {
                    return Ok(None);
                }
                let event = match event {
                    ServiceEvent::ServiceResolved(resolved) => {
                        record_from_resolved(&resolved).map(RawEvent::Upsert)
                    }
                    ServiceEvent::ServiceRemoved(service_type, fullname) => {
                        raw_event_from_removed(service_type, &fullname)
                    }
                    _ => None,
                };
                if event.is_some() {
                    return Ok(event);
                }
            }
        })
    }

    fn request_close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Err(error) = retry_daemon_command(|| self.daemon.stop_browse(&self.service_type)) {
            tracing::debug!(%error, "iroh-http-discovery: could not enqueue DNS-SD browse stop");
        }
        if let Err(error) = retry_daemon_command(|| self.daemon.shutdown()) {
            tracing::debug!(%error, "iroh-http-discovery: could not enqueue DNS-SD daemon shutdown");
        }
    }

    fn closed(&self) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(futures::future::ready(Ok(())))
    }
}

/// An active browse session that yields [`ServiceRecord`]s.
///
/// Drop or call [`Self::close`] to stop receiving records, stop the underlying
/// DNS-SD browse, and shut its daemon down.
pub struct BrowseSession {
    inner: Arc<crate::engine::BrowseSession>,
}

impl BrowseSession {
    /// Returns the next record, or `None` when the session is closed.
    ///
    /// This takes `&self`, so a session can be placed directly in an [`Arc`]
    /// and polled without an external asynchronous mutex.
    pub async fn next_record(&self) -> Option<ServiceRecord> {
        match self.inner.next().await {
            Ok(Some(event)) => Some(event.into()),
            Ok(None) => None,
            Err(error) => {
                tracing::debug!(%error, "iroh-http-discovery: DNS-SD browse failed");
                None
            }
        }
    }

    /// Synchronously stop this browse session.
    ///
    /// Closing is idempotent and asks the transport to wake any pending
    /// [`Self::next_record`] call.
    pub fn close(&self) {
        self.inner.close();
    }

    pub(crate) fn on_close(&self, callback: impl FnOnce() + Send + 'static) {
        self.inner.on_close(callback);
    }
}

/// Browse for services on the local network via DNS-SD.
///
/// Browses `_<service_name>.<proto>.local.` and yields a [`ServiceRecord`] for
/// each resolved or removed service.
pub fn browse(config: BrowseConfig) -> Result<BrowseSession, DiscoveryError> {
    let service_type = service_type(&config.service_name, config.protocol)?;
    let daemon = new_daemon()?;
    let receiver = match daemon.browse(&service_type) {
        Ok(receiver) => receiver,
        Err(error) => {
            let _ = retry_daemon_command(|| daemon.shutdown());
            return Err(DiscoveryError::Setup(error.to_string()));
        }
    };

    Ok(BrowseSession {
        inner: Arc::new(crate::engine::BrowseSession::new(MdnsBrowse {
            receiver,
            daemon,
            service_type,
            closed: AtomicBool::new(false),
        })),
    })
}

#[cfg(test)]
struct ControlledBrowse {
    close: tokio::sync::Semaphore,
}

#[cfg(test)]
impl BrowseHandle for ControlledBrowse {
    fn next(&self) -> BoxFuture<'_, Result<Option<RawEvent>, TransportError>> {
        Box::pin(async move {
            let _ = self.close.acquire().await;
            Ok(None)
        })
    }

    fn request_close(&self) {
        self.close.add_permits(1);
    }

    fn closed(&self) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(futures::future::ready(Ok(())))
    }
}

#[cfg(test)]
pub(crate) fn controlled_test_browse() -> (BrowseSession, futures::channel::oneshot::Sender<()>) {
    let (finish_tx, finish_rx) = futures::channel::oneshot::channel();
    let inner = Arc::new(crate::engine::BrowseSession::new(ControlledBrowse {
        close: tokio::sync::Semaphore::new(0),
    }));
    let weak = Arc::downgrade(&inner);
    tokio::spawn(async move {
        let _ = finish_rx.await;
        if let Some(inner) = weak.upgrade() {
            inner.close();
        }
    });
    (BrowseSession { inner }, finish_tx)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_type_builds_udp_local_domain() {
        assert_eq!(
            service_type("iroh-http", Protocol::Udp).unwrap(),
            "_iroh-http._udp.local."
        );
    }

    #[test]
    fn service_type_builds_tcp_local_domain() {
        assert_eq!(
            service_type("my-app", Protocol::Tcp).unwrap(),
            "_my-app._tcp.local."
        );
    }

    #[test]
    fn service_type_rejects_empty_and_illegal_names() {
        assert!(matches!(
            service_type("", Protocol::Udp),
            Err(DiscoveryError::InvalidServiceName(_))
        ));
        assert!(matches!(
            service_type("has space", Protocol::Udp),
            Err(DiscoveryError::InvalidServiceName(_))
        ));
        assert!(matches!(
            service_type("has.dot", Protocol::Udp),
            Err(DiscoveryError::InvalidServiceName(_))
        ));
    }

    #[test]
    fn instance_from_fullname_extracts_label() {
        let ty_domain = "_iroh-http._udp.local.";
        let full = "abcdef234567._iroh-http._udp.local.";
        assert_eq!(
            instance_from_fullname(full, ty_domain).as_deref(),
            Some("abcdef234567")
        );
        assert_eq!(
            instance_from_fullname("._iroh-http._udp.local.", ty_domain),
            None
        );
    }

    #[test]
    fn instance_from_fullname_handles_dotted_instance_label() {
        // Regression for PR #330 review finding #5: an instance label that
        // itself contains the two-character sequence `._` must not be
        // truncated by a naive first-match split — only the known
        // `ty_domain` suffix should be stripped.
        let ty_domain = "_my-app._udp.local.";
        let full = "foo._bar._my-app._udp.local.";
        assert_eq!(
            instance_from_fullname(full, ty_domain).as_deref(),
            Some("foo._bar")
        );
    }

    #[test]
    fn removed_record_has_no_addresses_or_txt() {
        let rec: ServiceRecord = raw_event_from_removed(
            "_my-app._udp.local.".to_string(),
            "inst._my-app._udp.local.",
        )
        .expect("valid instance")
        .into();
        assert!(!rec.is_active);
        assert_eq!(rec.instance_name, "inst");
        assert!(rec.addrs.is_empty());
        assert!(rec.txt.is_empty());
        assert_eq!(rec.port, 0);
        assert!(rec.host.is_none());
    }

    #[test]
    fn scoped_ipv6_socket_conversion_preserves_the_interface_index() {
        let socket = socket_addr_with_scope("fe80::1234".parse().unwrap(), 5353, 17);

        assert_eq!(socket.ip(), "fe80::1234".parse::<IpAddr>().unwrap());
        assert_eq!(socket.port(), 5353);
        let SocketAddr::V6(socket) = socket else {
            panic!("expected an IPv6 socket address");
        };
        assert_eq!(socket.scope_id(), 17);
    }

    #[test]
    fn non_link_local_ipv6_socket_conversion_drops_incidental_scope() {
        let ip = "fd00::1234".parse().unwrap();
        let socket = socket_addr_with_scope(IpAddr::V6(ip), 5353, applicable_ipv6_scope(&ip, 17));

        let SocketAddr::V6(socket) = socket else {
            panic!("expected an IPv6 socket address");
        };
        assert_eq!(socket.scope_id(), 0);
    }

    #[test]
    fn changed_advertisement_rebuilds_the_record_and_deduplicates_snapshots() {
        let initial = ServiceConfig {
            service_name: "iroh-http".to_string(),
            instance_name: "peer".to_string(),
            port: 4433,
            addrs: vec!["192.168.1.2".parse().unwrap()],
            txt: vec![("address".to_string(), "192.168.1.2:4433".to_string())],
            protocol: Protocol::Udp,
        };
        let updated = ServiceConfig {
            addrs: vec!["192.168.1.3".parse().unwrap(), "fd00::3".parse().unwrap()],
            txt: vec![(
                "address".to_string(),
                "192.168.1.3:4433,[fd00::3]:4433".to_string(),
            )],
            ..initial.clone()
        };
        let mut current = Some(initial);
        let mut registered = Vec::new();

        assert!(
            register_changed(&mut current, updated.clone(), true, |info| {
                registered.push(info);
                Ok(())
            })
            .unwrap()
        );
        assert_eq!(registered.len(), 1);
        assert_eq!(
            registered[0].get_property_val_str("address"),
            Some("192.168.1.3:4433,[fd00::3]:4433")
        );
        assert!(!register_changed(&mut current, updated, true, |info| {
            registered.push(info);
            Ok(())
        })
        .unwrap());
        assert_eq!(
            registered.len(),
            1,
            "an unchanged snapshot is not re-announced"
        );
    }

    #[test]
    fn failed_unregister_enqueue_keeps_registration_state_for_retry() {
        let config = ServiceConfig {
            service_name: "iroh-http".to_string(),
            instance_name: "peer".to_string(),
            port: 4433,
            addrs: vec!["192.168.1.2".parse().unwrap()],
            txt: Vec::new(),
            protocol: Protocol::Udp,
        };
        let mut current = Some(config);

        let first = retire_after_enqueue(&mut current, || Err::<(), _>("queue full"));
        assert_eq!(first, Err("queue full"));
        assert!(current.is_some(), "a failed enqueue remains retryable");

        retire_after_enqueue(&mut current, || Ok::<_, &str>(())).unwrap();
        assert!(current.is_none(), "successful retry retires the state");
    }

    #[tokio::test]
    async fn close_is_idempotent_and_wakes_a_shared_pending_next() {
        let (session, _finish) = controlled_test_browse();
        let session = Arc::new(session);
        let waiting = tokio::spawn({
            let session = session.clone();
            async move { session.next_record().await }
        });
        tokio::task::yield_now().await;

        session.close();
        session.close();

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("close must wake next_record")
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn unexpected_pump_terminal_wakes_and_closes_the_session() {
        let (session, finish) = controlled_test_browse();
        let session = Arc::new(session);
        let waiting = tokio::spawn({
            let session = session.clone();
            async move { session.next_record().await }
        });
        tokio::task::yield_now().await;

        finish.send(()).unwrap();

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("pump terminal must wake next_record")
            .unwrap();
        assert!(result.is_none());
        session.close();
    }

    #[test]
    fn refresh_worker_needs_no_tokio_context_and_finalizes_every_terminal_path() {
        use std::future::pending;
        use std::sync::mpsc;
        use std::time::Duration;

        struct DropNotify(Option<mpsc::Sender<()>>);

        impl Drop for DropNotify {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }

        let (close_tx, close_rx) = futures::channel::oneshot::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let (closed_drop_tx, closed_drop_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let mut closed_worker = spawn_refresh_until_closed(
            async move {
                let _ = close_rx.await;
            },
            async move {
                started_tx.send(()).unwrap();
                let _notify = DropNotify(Some(closed_drop_tx));
                pending::<()>().await;
            },
            move || finished_tx.send(()).unwrap(),
        )
        .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        close_tx.send(()).unwrap();
        closed_drop_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closed_worker.cancel();

        let (abort_started_tx, abort_started_rx) = mpsc::channel();
        let (abort_drop_tx, abort_drop_rx) = mpsc::channel();
        let mut cancelled_worker = spawn_refresh_until_closed(
            pending::<()>(),
            async move {
                abort_started_tx.send(()).unwrap();
                let _notify = DropNotify(Some(abort_drop_tx));
                pending::<()>().await;
            },
            || {},
        )
        .unwrap();
        abort_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        cancelled_worker.cancel();
        abort_drop_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (refresh_finished_tx, refresh_finished_rx) = mpsc::channel();
        let mut terminal_worker =
            spawn_refresh_until_closed(pending::<()>(), futures::future::ready(()), move || {
                refresh_finished_tx.send(()).unwrap()
            })
            .unwrap();
        refresh_finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("refresh termination must run the unregister finalizer");
        terminal_worker.cancel();
    }
}
