//! Temporary compatibility for scoped IPv6 DNS nameservers.
//!
//! This module exists because released `iroh-dns` accepts a full
//! `SocketAddrV6` and then its Hickory adapter reduces it to `addr.ip()`,
//! discarding the interface scope required to route link-local nameservers.
//! See ADR-013 and the removal tracker:
//! <https://github.com/momics/iroh-http/issues/368>.
//!
//! Upstream ownership:
//! - <https://github.com/hickory-dns/hickory-dns/issues/3713>
//! - <https://github.com/n0-computer/iroh/pull/4210>
//! - <https://github.com/n0-computer/iroh/pull/4419>
//!
//! Per ADR-013, `iroh-dns` and its resolver transport are the ecosystem
//! implementation we should use. The concrete incompatibility is that the
//! released transport cannot preserve a destination scope ID. This private
//! UDP-only forwarder is therefore a release bridge, not a supported DNS
//! implementation. It deliberately does not add TCP fallback. Delete this
//! module once a released upstream resolver preserves scoped socket addresses
//! and the physical-device gates in issue #368 pass.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

#[cfg(test)]
use super::IrohEndpoint;

/// Owns all temporary resolver helpers for one endpoint.
///
/// Explicit shutdown is required because callers may retain a closed endpoint;
/// `Drop` remains the failed-bind and final-release fallback.
#[derive(Default)]
pub(super) struct ScopedDnsCompat {
    proxies: Vec<ScopedDnsProxy>,
}

impl ScopedDnsCompat {
    /// Return the address that the released resolver can consume.
    ///
    /// Ordinary addresses pass through unchanged. Scoped IPv6 destinations
    /// retain their exact address behind the temporary loopback forwarder.
    pub(super) async fn resolver_addr(
        &mut self,
        nameserver: &str,
        addr: SocketAddr,
    ) -> Result<SocketAddr, crate::CoreError> {
        let SocketAddr::V6(v6) = addr else {
            return Ok(addr);
        };
        if v6.scope_id() == 0 {
            return Ok(addr);
        }

        let proxy = start_scoped_dns_proxy(addr).await.map_err(|reason| {
            crate::CoreError::connection_failed(format!(
                "failed to start scoped DNS proxy for {nameserver:?}: {reason}"
            ))
        })?;
        let local_addr = proxy.local_addr();
        self.proxies.push(proxy);
        Ok(local_addr)
    }

    pub(super) async fn shutdown(&self) {
        for proxy in &self.proxies {
            proxy.shutdown().await;
        }
    }

    #[cfg(test)]
    pub(super) fn proxy_count(&self) -> usize {
        self.proxies.len()
    }

    #[cfg(test)]
    pub(super) fn running_proxy_count(&self) -> usize {
        self.proxies
            .iter()
            .filter(|proxy| proxy.is_running())
            .count()
    }
}

/// Per-endpoint forwarding proxy for a scoped IPv6 DNS nameserver.
///
/// `iroh-dns`/Hickory preserves a configured nameserver's port but represents
/// its destination as an `IpAddr`, which cannot carry an IPv6 scope ID. The
/// resolver therefore talks to `local_addr`, while this task forwards each
/// datagram to the exact scoped upstream address. Dropping the handle aborts
/// the listener and all in-flight forwards.
const MAX_SCOPED_DNS_IN_FLIGHT: usize = 32;
/// Practical EDNS UDP ceiling. One extra byte is allocated while receiving so
/// a truncated oversized datagram is distinguishable from an exact-size one.
const MAX_SCOPED_DNS_UDP_PAYLOAD: usize = 4096;

#[derive(Default)]
struct ScopedDnsProxyLoad {
    in_flight: AtomicUsize,
    dropped_queries: AtomicUsize,
}

struct InFlightDnsQuery(Arc<ScopedDnsProxyLoad>);

impl InFlightDnsQuery {
    fn begin(load: Arc<ScopedDnsProxyLoad>) -> Self {
        load.in_flight.fetch_add(1, Ordering::Relaxed);
        Self(load)
    }
}

impl Drop for InFlightDnsQuery {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

struct ScopedDnsProxy {
    local_addr: SocketAddr,
    _upstream: SocketAddr,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    #[cfg(test)]
    abort_handle: tokio::task::AbortHandle,
    _load: Arc<ScopedDnsProxyLoad>,
}

impl ScopedDnsProxy {
    fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    #[cfg(test)]
    fn upstream(&self) -> SocketAddr {
        self._upstream
    }

    #[cfg(test)]
    fn in_flight(&self) -> usize {
        self._load.in_flight.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn dropped_queries(&self) -> usize {
        self._load.dropped_queries.load(Ordering::Relaxed)
    }

    async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        let mut task = self.task.lock().await;
        if let Some(task) = task.as_mut() {
            let _ = task.await;
        }
        task.take();
    }

    #[cfg(test)]
    fn abort_handle(&self) -> tokio::task::AbortHandle {
        self.abort_handle.clone()
    }

    #[cfg(test)]
    fn is_running(&self) -> bool {
        !self.abort_handle.is_finished()
    }
}

impl Drop for ScopedDnsProxy {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

async fn start_scoped_dns_proxy(upstream: SocketAddr) -> io::Result<ScopedDnsProxy> {
    let SocketAddr::V6(v6) = upstream else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scoped DNS proxy requires an IPv6 upstream",
        ));
    };
    if v6.scope_id() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scoped DNS proxy requires a non-zero IPv6 scope ID",
        ));
    }

    // Hickory retains this ephemeral port. IPv4 loopback is deliberate: the
    // resolver-facing hop needs no scope and works even when the upstream is
    // reachable only over IPv6.
    let listener = Arc::new(tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?);
    let local_addr = listener.local_addr()?;
    let load = Arc::new(ScopedDnsProxyLoad::default());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(run_scoped_dns_proxy(
        listener,
        upstream,
        Arc::clone(&load),
        shutdown_rx,
    ));
    #[cfg(test)]
    let abort_handle = task.abort_handle();
    Ok(ScopedDnsProxy {
        local_addr,
        _upstream: upstream,
        shutdown_tx,
        task: tokio::sync::Mutex::new(Some(task)),
        #[cfg(test)]
        abort_handle,
        _load: load,
    })
}

async fn run_scoped_dns_proxy(
    listener: Arc<tokio::net::UdpSocket>,
    upstream: SocketAddr,
    load: Arc<ScopedDnsProxyLoad>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut query = vec![0u8; MAX_SCOPED_DNS_UDP_PAYLOAD + 1];
    let mut forwards = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                break;
            }
            received = listener.recv_from(&mut query) => {
                let (len, client) = match received {
                    Ok(received) => received,
                    Err(reason) => {
                        tracing::debug!(%upstream, %reason, "scoped DNS proxy listener stopped");
                        break;
                    }
                };
                if len > MAX_SCOPED_DNS_UDP_PAYLOAD {
                    load.dropped_queries.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(%upstream, %client, len, "scoped DNS proxy dropping oversized query");
                    continue;
                }
                // A completed child has already released live capacity, but
                // its JoinSet entry still owns metadata until reaped. Drain all
                // ready entries on the hot receive path to bound both live work
                // and retained completion records under sustained traffic.
                while let Some(completed) = forwards.try_join_next() {
                    if let Err(reason) = completed {
                        tracing::debug!(%upstream, %reason, "scoped DNS forwarding task failed");
                    }
                }
                // Completed children release capacity before the JoinSet entry
                // is reaped; use the live-work counter rather than `len()` so
                // a busy listener cannot falsely remain at capacity.
                if load.in_flight.load(Ordering::Relaxed) >= MAX_SCOPED_DNS_IN_FLIGHT {
                    load.dropped_queries.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(%upstream, %client, "scoped DNS proxy at capacity; dropping query");
                    continue;
                }
                let listener = Arc::clone(&listener);
                let query = query[..len].to_vec();
                let in_flight = InFlightDnsQuery::begin(Arc::clone(&load));
                forwards.spawn(async move {
                    let _in_flight = in_flight;
                    if let Err(reason) = forward_dns_datagram(&listener, upstream, client, &query).await {
                        tracing::debug!(%upstream, %client, %reason, "scoped DNS query forwarding failed");
                    }
                });
            }
            Some(completed) = forwards.join_next(), if !forwards.is_empty() => {
                if let Err(reason) = completed {
                    tracing::debug!(%upstream, %reason, "scoped DNS forwarding task failed");
                }
            }
        }
    }

    // Explicit close awaits this drain. Dropping the proxy retains the cheaper
    // abort-only fallback for runtimes where async cleanup is no longer possible.
    forwards.abort_all();
    while let Some(completed) = forwards.join_next().await {
        if let Err(reason) = completed {
            if !reason.is_cancelled() {
                tracing::debug!(%upstream, %reason, "scoped DNS forwarding task failed during shutdown");
            }
        }
    }
}

async fn forward_dns_datagram(
    listener: &tokio::net::UdpSocket,
    upstream: SocketAddr,
    client: SocketAddr,
    query: &[u8],
) -> io::Result<()> {
    let bind_addr = match upstream {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let outbound = tokio::net::UdpSocket::bind(bind_addr).await?;
    // A connected UDP socket retains the IPv6 scope on the destination and
    // makes the kernel discard datagrams from every other source.
    outbound.connect(upstream).await?;
    outbound.send(query).await?;

    let mut response = vec![0u8; MAX_SCOPED_DNS_UDP_PAYLOAD + 1];
    let len = tokio::time::timeout(iroh::dns::DNS_TIMEOUT, outbound.recv(&mut response))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "DNS upstream timed out"))??;
    if len > MAX_SCOPED_DNS_UDP_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "DNS upstream response exceeded the proxy payload cap",
        ));
    }
    listener.send_to(&response[..len], client).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
        sync::{atomic::Ordering, Arc},
        time::Duration,
    };

    use super::{
        start_scoped_dns_proxy, IrohEndpoint, ScopedDnsCompat, MAX_SCOPED_DNS_IN_FLIGHT,
        MAX_SCOPED_DNS_UDP_PAYLOAD,
    };
    use crate::endpoint::config::{DiscoveryOptions, NodeOptions};

    fn a_record_response(query: &[u8], answer: Ipv4Addr) -> Vec<u8> {
        assert!(query.len() >= 12, "DNS query header is truncated");
        let mut question_end = 12;
        while query[question_end] != 0 {
            question_end += usize::from(query[question_end]) + 1;
            assert!(question_end < query.len(), "DNS query name is truncated");
        }
        question_end += 5; // root label + QTYPE + QCLASS
        assert!(question_end <= query.len(), "DNS question is truncated");

        let mut response = Vec::with_capacity(question_end + 16);
        response.extend_from_slice(&query[..2]); // transaction ID
        response.extend_from_slice(&[0x81, 0x80]); // response, recursion available, no error
        response.extend_from_slice(&[0, 1]); // one question
        response.extend_from_slice(&[0, 1]); // one answer
        response.extend_from_slice(&[0, 0, 0, 0]); // no authority/additional records
        response.extend_from_slice(&query[12..question_end]);
        response.extend_from_slice(&[
            0xc0, 0x0c, // compressed owner name
            0, 1, // A
            0, 1, // IN
            0, 0, 0, 60, // TTL
            0, 4, // RDLENGTH
        ]);
        response.extend_from_slice(&answer.octets());
        response
    }

    #[tokio::test]
    async fn scoped_proxy_integrates_with_iroh_dns_and_retains_configured_scope() {
        // Loopback does not exercise kernel scope routing; the stored-address
        // assertion proves plumbing, while issue #368 retains the physical-device gate.
        let upstream = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind fake IPv6 DNS server");
        let upstream_port = upstream.local_addr().unwrap().port();
        let exact_upstream =
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, upstream_port, 0, 17));
        let expected = Ipv4Addr::new(192, 0, 2, 44);
        let upstream_task = tokio::spawn(async move {
            let mut query = [0u8; 4096];
            let (len, peer) = upstream
                .recv_from(&mut query)
                .await
                .expect("receive forwarded DNS query");
            let response = a_record_response(&query[..len], expected);
            upstream
                .send_to(&response, peer)
                .await
                .expect("send fake DNS response");
        });

        let proxy = start_scoped_dns_proxy(exact_upstream)
            .await
            .expect("start scoped DNS proxy");
        assert_eq!(
            proxy.upstream(),
            exact_upstream,
            "the forwarding destination must retain its non-zero scope ID"
        );

        let resolver = iroh::dns::DnsResolver::with_nameserver(proxy.local_addr());
        let answers: Vec<_> = resolver
            .lookup_ipv4("scoped.test.", Duration::from_secs(2))
            .await
            .expect("actual iroh DNS lookup through proxy")
            .collect();

        assert_eq!(answers, [IpAddr::V4(expected)]);
        upstream_task.await.expect("fake DNS server task");
    }

    #[tokio::test]
    async fn scoped_proxy_load_sheds_flood_when_upstream_never_responds() {
        // A bound-but-silent socket deterministically accepts every forwarded
        // datagram without producing ICMP errors or DNS responses. Every
        // accepted proxy job therefore remains in flight until timeout.
        let silent_upstream = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind silent IPv6 upstream");
        let upstream = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::LOCALHOST,
            silent_upstream.local_addr().unwrap().port(),
            0,
            17,
        ));
        let proxy = start_scoped_dns_proxy(upstream)
            .await
            .expect("start scoped DNS proxy");
        let client = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind flood client");
        let query = [0u8; 12];

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                for _ in 0..8 {
                    client
                        .send_to(&query, proxy.local_addr())
                        .await
                        .expect("send local DNS flood packet");
                }
                tokio::task::yield_now().await;
                if proxy.in_flight() == MAX_SCOPED_DNS_IN_FLIGHT && proxy.dropped_queries() > 0 {
                    break;
                }
            }
        })
        .await
        .expect("proxy must reach its bound and load-shed promptly");

        assert_eq!(proxy.in_flight(), MAX_SCOPED_DNS_IN_FLIGHT);
        assert!(proxy.dropped_queries() > 0);

        tokio::time::timeout(
            iroh::dns::DNS_TIMEOUT.saturating_add(Duration::from_secs(1)),
            async {
                while proxy.in_flight() != 0 {
                    tokio::task::yield_now().await;
                }
            },
        )
        .await
        .expect("timed-out queries must release every capacity slot");

        client
            .send_to(&query, proxy.local_addr())
            .await
            .expect("send query after capacity recovery");
        tokio::time::timeout(Duration::from_secs(1), async {
            while proxy.in_flight() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("proxy must accept work after timed-out slots are released");
        drop(silent_upstream);
    }

    #[tokio::test]
    async fn dropping_proxy_aborts_listener_task() {
        let silent_upstream = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind silent IPv6 upstream");
        let upstream = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::LOCALHOST,
            silent_upstream
                .local_addr()
                .expect("silent upstream address")
                .port(),
            0,
            17,
        ));
        let proxy = start_scoped_dns_proxy(upstream)
            .await
            .expect("start scoped DNS proxy");
        let abort_handle = proxy.abort_handle();
        let load = Arc::clone(&proxy._load);
        let client = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind local DNS client");
        client
            .send_to(&[0u8; 12], proxy.local_addr())
            .await
            .expect("send in-flight DNS query");
        tokio::time::timeout(Duration::from_secs(1), async {
            while load.in_flight.load(Ordering::Relaxed) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("query must become in-flight before Drop");

        drop(proxy);

        tokio::time::timeout(Duration::from_secs(1), async {
            while !abort_handle.is_finished() || load.in_flight.load(Ordering::Relaxed) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Drop must abort the proxy listener and its JoinSet");
    }

    #[tokio::test]
    async fn concurrent_shutdown_waits_for_in_flight_forwarders() {
        let silent_upstream = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind silent IPv6 upstream");
        let upstream = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::LOCALHOST,
            silent_upstream
                .local_addr()
                .expect("silent upstream address")
                .port(),
            0,
            17,
        ));
        let proxy = start_scoped_dns_proxy(upstream)
            .await
            .expect("start scoped DNS proxy");
        let load = Arc::clone(&proxy._load);
        let client = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind local DNS client");
        client
            .send_to(&[0u8; 12], proxy.local_addr())
            .await
            .expect("send in-flight DNS query");
        tokio::time::timeout(Duration::from_secs(1), async {
            while load.in_flight.load(Ordering::Relaxed) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("query must become in-flight before shutdown");

        let compat = Arc::new(ScopedDnsCompat {
            proxies: vec![proxy],
        });
        let first = compat.shutdown();
        let second = compat.shutdown();
        tokio::join!(first, second);

        assert_eq!(
            load.in_flight.load(Ordering::Relaxed),
            0,
            "every close waiter must observe forwarding-task termination"
        );
    }

    #[tokio::test]
    async fn scoped_proxy_drops_oversized_dns_datagrams() {
        let upstream = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind fake IPv6 upstream");
        let upstream_addr = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::LOCALHOST,
            upstream.local_addr().unwrap().port(),
            0,
            17,
        ));
        let proxy = start_scoped_dns_proxy(upstream_addr)
            .await
            .expect("start scoped DNS proxy");
        let client = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind local DNS client");

        client
            .send_to(
                &vec![0u8; MAX_SCOPED_DNS_UDP_PAYLOAD + 1],
                proxy.local_addr(),
            )
            .await
            .expect("send oversized local DNS datagram");
        tokio::time::timeout(Duration::from_secs(1), async {
            while proxy.dropped_queries() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("oversized datagram must be dropped promptly");

        assert_eq!(proxy.in_flight(), 0);
        let mut forwarded = vec![0u8; MAX_SCOPED_DNS_UDP_PAYLOAD + 1];
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                upstream.recv_from(&mut forwarded)
            )
            .await
            .is_err(),
            "oversized DNS datagram must never reach the upstream"
        );
    }

    #[tokio::test]
    async fn retained_endpoint_close_modes_stop_scoped_dns_proxy() {
        async fn assert_close_mode(force: bool) {
            let discovery = {
                let mut discovery = DiscoveryOptions::new(None, false);
                discovery.dns_nameservers = vec!["fe80::1%17".to_string()];
                discovery
            };
            let mut opts = NodeOptions {
                discovery,
                ..Default::default()
            };
            opts.networking.disabled = true;
            let ep = IrohEndpoint::bind(opts)
                .await
                .expect("bind retained endpoint with scoped DNS proxy");

            assert_eq!(ep.inner.transport.scoped_dns_proxy_count(), 1);
            assert_eq!(ep.inner.transport.running_scoped_dns_proxy_count(), 1);
            let retained_node_id = ep.node_id().to_string();
            if force {
                ep.close_force().await;
            } else {
                ep.close().await;
            }

            assert_eq!(
                ep.inner.transport.running_scoped_dns_proxy_count(),
                0,
                "close must await proxy shutdown while endpoint is retained"
            );
            assert_eq!(ep.node_id(), retained_node_id);
        }

        assert_close_mode(false).await;
        assert_close_mode(true).await;
    }
}
