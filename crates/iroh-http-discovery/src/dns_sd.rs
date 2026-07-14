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

use crate::DiscoveryError;

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

// ── Protocol ─────────────────────────────────────────────────────────────────

/// Transport protocol component of a DNS-SD service type (`_udp` / `_tcp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    /// `_udp` — the default, and what iroh-http uses (QUIC is UDP-based).
    #[default]
    Udp,
    /// `_tcp` — for advertising/browsing TCP-based services.
    Tcp,
}

impl Protocol {
    /// The DNS-SD label for this protocol, including the leading underscore.
    fn label(self) -> &'static str {
        match self {
            Protocol::Udp => "_udp",
            Protocol::Tcp => "_tcp",
        }
    }
}

// ── Config / record types ────────────────────────────────────────────────────

/// A service to advertise via DNS-SD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    /// Bare service name, e.g. `"my-app"` → `_my-app._udp.local.`. Must be a
    /// single DNS label: non-empty, ASCII alphanumeric or `-`.
    pub service_name: String,
    /// DNS-SD instance label (the human/instance-visible name).
    pub instance_name: String,
    /// SRV port peers should connect to.
    pub port: u16,
    /// Direct IP addresses to advertise. Local interface addresses are always
    /// added automatically and kept fresh; entries here are advertised in
    /// addition.
    pub addrs: Vec<IpAddr>,
    /// Arbitrary TXT key/value properties.
    pub txt: Vec<(String, String)>,
    /// Transport protocol (`_udp` default).
    pub protocol: Protocol,
}

/// Parameters for a browse session.
#[derive(Debug, Clone)]
pub struct BrowseConfig {
    /// Bare service name to browse, e.g. `"my-app"` → `_my-app._udp.local.`.
    pub service_name: String,
    /// Transport protocol (`_udp` default).
    pub protocol: Protocol,
}

impl BrowseConfig {
    /// Convenience constructor for a `_udp` browse of `service_name`.
    pub fn udp(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            protocol: Protocol::Udp,
        }
    }
}

/// A fully-resolved DNS-SD service record — nothing dropped.
#[derive(Debug, Clone)]
pub struct ServiceRecord {
    /// `true` = the service appeared or updated; `false` = it expired/left.
    pub is_active: bool,
    /// Service type and domain, e.g. `_my-app._udp.local.`.
    pub service_type: String,
    /// Service instance label.
    pub instance_name: String,
    /// Host name of the service, e.g. `my-host.local.` (absent on expiry).
    pub host: Option<String>,
    /// SRV port (`0` on expiry).
    pub port: u16,
    /// Resolved socket addresses (empty on expiry).
    pub addrs: Vec<SocketAddr>,
    /// All TXT key/value properties (empty on expiry).
    pub txt: Vec<(String, String)>,
}

// ── Service-type helpers ─────────────────────────────────────────────────────

/// Validate a service name and build the fully-qualified DNS-SD service type
/// `_<service_name>.<proto>.local.`.
///
/// The name must be a single DNS label: non-empty and ASCII alphanumeric or
/// `-` (no leading `_`, no dot).
pub(crate) fn service_type(
    service_name: &str,
    protocol: Protocol,
) -> Result<String, DiscoveryError> {
    if service_name.is_empty() {
        return Err(DiscoveryError::InvalidServiceName(
            "service name must not be empty".into(),
        ));
    }
    if !service_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(DiscoveryError::InvalidServiceName(format!(
            "{service_name:?} must contain only ASCII letters, digits, and '-'"
        )));
    }
    Ok(format!("_{service_name}.{}.local.", protocol.label()))
}

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

fn record_from_removed(service_type: String, fullname: &str) -> Option<ServiceRecord> {
    let instance_name = instance_from_fullname(fullname, &service_type)?;
    Some(ServiceRecord {
        is_active: false,
        service_type,
        instance_name,
        host: None,
        port: 0,
        addrs: Vec::new(),
        txt: Vec::new(),
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

struct BrowseShared {
    rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<ServiceRecord>>,
    daemon: Option<ServiceDaemon>,
    service_type: String,
    pump: Mutex<Option<tokio::task::AbortHandle>>,
    closed: AtomicBool,
    closed_notify: tokio::sync::Notify,
    close_callbacks: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl BrowseShared {
    fn stop_daemon(&self) {
        if let Some(daemon) = &self.daemon {
            let _ = retry_daemon_command(|| daemon.stop_browse(&self.service_type));
            let _ = retry_daemon_command(|| daemon.shutdown());
        }
    }

    fn run_close_callbacks(&self) {
        let callbacks = std::mem::take(
            &mut *self
                .close_callbacks
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for callback in callbacks {
            callback();
        }
    }

    fn install_pump(&self, pump: tokio::task::AbortHandle) {
        let mut slot = self.pump.lock().unwrap_or_else(|error| error.into_inner());
        if self.closed.load(Ordering::Acquire) {
            pump.abort();
        } else {
            *slot = Some(pump);
        }
    }

    fn finish_from_pump(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stop_daemon();
        self.closed_notify.notify_waiters();
        self.run_close_callbacks();
        self.pump
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stop_daemon();
        self.closed_notify.notify_waiters();
        self.run_close_callbacks();
        if let Some(pump) = self
            .pump
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            pump.abort();
        }
    }
}

/// An active browse session that yields [`ServiceRecord`]s.
///
/// Drop or call [`Self::close`] to stop receiving records, stop the underlying
/// DNS-SD browse, shut its daemon down, and stop the record pump.
pub struct BrowseSession {
    shared: Arc<BrowseShared>,
}

impl BrowseSession {
    /// Returns the next record, or `None` when the session is closed.
    ///
    /// This takes `&self`, so a session can be placed directly in an [`Arc`]
    /// and polled without an external asynchronous mutex.
    pub async fn next_record(&self) -> Option<ServiceRecord> {
        let closed = self.shared.closed_notify.notified();
        tokio::pin!(closed);
        closed.as_mut().enable();
        if self.shared.closed.load(Ordering::Acquire) {
            return None;
        }
        let record = {
            let mut rx = self.shared.rx.lock().await;
            if self.shared.closed.load(Ordering::Acquire) {
                None
            } else {
                tokio::select! {
                    record = rx.recv() => record,
                    _ = &mut closed => None,
                }
            }
        };
        if self.shared.closed.load(Ordering::Acquire) {
            None
        } else {
            record
        }
    }

    /// Synchronously stop this browse session.
    ///
    /// Closing is idempotent and aborts the record pump, which drops the sole
    /// channel sender and wakes any pending [`Self::next_record`] call.
    pub fn close(&self) {
        self.shared.close();
    }

    pub(crate) fn on_close(&self, callback: impl FnOnce() + Send + 'static) {
        let mut callbacks = self
            .shared
            .close_callbacks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.shared.closed.load(Ordering::Acquire) {
            drop(callbacks);
            callback();
        } else {
            callbacks.push(Box::new(callback));
        }
    }
}

impl Drop for BrowseSession {
    fn drop(&mut self) {
        self.close();
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

    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let shared = Arc::new(BrowseShared {
        rx: tokio::sync::Mutex::new(rx),
        daemon: Some(daemon),
        service_type,
        pump: Mutex::new(None),
        closed: AtomicBool::new(false),
        closed_notify: tokio::sync::Notify::new(),
        close_callbacks: Mutex::new(Vec::new()),
    });
    let weak = Arc::downgrade(&shared);
    let pump = tokio::spawn(async move {
        while let Ok(event) = receiver.recv_async().await {
            let record = match event {
                ServiceEvent::ServiceResolved(rs) => record_from_resolved(&rs),
                ServiceEvent::ServiceRemoved(ty, fullname) => record_from_removed(ty, &fullname),
                _ => None,
            };
            if let Some(record) = record {
                if tx.send(record).await.is_err() {
                    break;
                }
            }
        }
        if let Some(shared) = weak.upgrade() {
            shared.finish_from_pump();
        }
    });
    shared.install_pump(pump.abort_handle());
    drop(pump);

    Ok(BrowseSession { shared })
}

#[cfg(test)]
pub(crate) fn controlled_test_browse() -> (BrowseSession, futures::channel::oneshot::Sender<()>) {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let (finish_tx, finish_rx) = futures::channel::oneshot::channel();
    let shared = Arc::new(BrowseShared {
        rx: tokio::sync::Mutex::new(rx),
        daemon: None,
        service_type: "_test._udp.local.".to_string(),
        pump: Mutex::new(None),
        closed: AtomicBool::new(false),
        closed_notify: tokio::sync::Notify::new(),
        close_callbacks: Mutex::new(Vec::new()),
    });
    let weak = Arc::downgrade(&shared);
    let pump = tokio::spawn(async move {
        let _ = finish_rx.await;
        drop(tx);
        if let Some(shared) = weak.upgrade() {
            shared.finish_from_pump();
        }
    });
    shared.install_pump(pump.abort_handle());
    drop(pump);
    (BrowseSession { shared }, finish_tx)
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
        let rec = record_from_removed(
            "_my-app._udp.local.".to_string(),
            "inst._my-app._udp.local.",
        )
        .expect("valid instance");
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
