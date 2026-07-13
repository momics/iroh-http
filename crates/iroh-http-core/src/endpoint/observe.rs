//! Observability and peer-info methods on [`super::IrohEndpoint`].
//!
//! Split from `mod.rs` so the facade file stays ≤ 200 LoC. These methods
//! are read-only and have no lifecycle side effects.

use std::sync::atomic::Ordering;

use iroh::endpoint::TransportAddrUsage;

use super::{
    stats::{EndpointStats, NodeAddrInfo, PathInfo, PeerStats},
    IrohEndpoint,
};

impl IrohEndpoint {
    /// Snapshot of current endpoint statistics.
    ///
    /// All fields are point-in-time reads and may change between calls.
    pub fn endpoint_stats(&self) -> EndpointStats {
        let (active_readers, active_writers, active_sessions, total_handles) =
            self.inner.ffi.handles.count_handles();
        let pool_size = self.inner.http.pool.entry_count_approx() as usize;
        let active_connections = self.inner.http.active_connections.load(Ordering::Relaxed);
        let active_requests = self.inner.http.active_requests.load(Ordering::Relaxed);
        let active_path_subscriptions = self.inner.session.path_subs.len();
        let active_path_watchers = self
            .inner
            .session
            .active_path_watchers
            .load(Ordering::Relaxed);
        EndpointStats {
            active_readers,
            active_writers,
            active_sessions,
            total_handles,
            pool_size,
            active_connections,
            active_requests,
            active_path_subscriptions,
            active_path_watchers,
        }
    }

    /// Returns the local socket addresses this endpoint is bound to.
    pub fn bound_sockets(&self) -> Vec<std::net::SocketAddr> {
        self.inner.transport.ep.bound_sockets()
    }

    /// Whether the underlying QUIC transport is still usable.
    ///
    /// This is deliberately distinct from "the endpoint handle exists". A
    /// handle can keep resolving from the registry long after the transport
    /// behind it has been torn down (`close`/`close_force`) or — on mobile —
    /// after the OS has invalidated the socket during suspension. Mobile
    /// foreground recovery must key off this, not handle existence: see #336,
    /// where a half-live iOS node reported healthy because its handle survived,
    /// leaving desktop peers hanging until a long timeout.
    ///
    /// Returns `false` once the endpoint is closed or has no bound sockets.
    /// A `true` result means the transport has not been observed as dead; it
    /// is a necessary, not a sufficient, signal, so callers on mobile should
    /// still bound their requests with a timeout and treat repeated failures
    /// as a trigger to recreate the endpoint.
    pub fn transport_alive(&self) -> bool {
        !self.inner.transport.ep.is_closed() && !self.bound_sockets().is_empty()
    }

    /// Reconciled direct socket addresses for this endpoint.
    ///
    /// These are the endpoint's derived local IP addresses with their ports
    /// reconciled against the real bound sockets, so a platform that enumerates
    /// an address with port 0 (iOS, #346) yields the real bound QUIC port. Any
    /// address that cannot be given a usable port is omitted. Relay URLs are not
    /// included. This is the direct-address list that should be advertised.
    pub fn direct_socket_addrs(&self) -> Vec<std::net::SocketAddr> {
        let candidates: Vec<std::net::SocketAddr> =
            self.inner.transport.ep.addr().ip_addrs().copied().collect();
        let bound = self.bound_sockets();
        super::bind::reconcile_direct_addr_ports(&candidates, &bound)
    }

    /// Full node address: node ID + relay URL(s) + direct socket addresses.
    ///
    /// Direct-address ports are reconciled against the endpoint's bound sockets
    /// so a platform that enumerates a local address with port 0 (iOS, #346)
    /// still advertises the real bound QUIC port. Any address that cannot be
    /// given a usable port is dropped rather than advertised as an undialable
    /// `:0`.
    pub fn node_addr(&self) -> NodeAddrInfo {
        let addr = self.inner.transport.ep.addr();
        let mut addrs = Vec::new();
        for relay in addr.relay_urls() {
            addrs.push(relay.to_string());
        }
        for da in self.direct_socket_addrs() {
            addrs.push(da.to_string());
        }
        NodeAddrInfo {
            id: self.inner.transport.node_id_str.clone(),
            addrs,
        }
    }

    /// The first routable dialable direct address as `ip:port`, or `None` when
    /// only loopback / unspecified / link-local addresses are available.
    ///
    /// Ports are already reconciled against the real bound QUIC socket by
    /// [`Self::direct_socket_addrs`], so the returned address carries the true
    /// bound port rather than a platform placeholder such as iOS's `:0`/`:1`
    /// (#346). Routability filtering mirrors `select_advertise_address` in
    /// `iroh-http-discovery` (#350 W1): a link-local address is only valid on
    /// its own segment, so advertising it as a dialable address makes a browsing
    /// peer's direct dial fail. Generic `advertise` callers publish this value
    /// in the `address` TXT so peers can direct-dial over the LAN instead of
    /// falling back to relay-only.
    pub fn dialable_direct_address(&self) -> Option<String> {
        select_dialable_direct(&self.direct_socket_addrs())
    }

    /// Home relay URL, or `None` if not connected to a relay.
    pub fn home_relay(&self) -> Option<String> {
        self.inner
            .transport
            .ep
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string())
    }

    /// Known addresses for a remote peer, or `None` if not in the endpoint's cache.
    pub async fn peer_info(&self, node_id_b32: &str) -> Option<NodeAddrInfo> {
        let bytes = crate::base32_decode(node_id_b32).ok()?;
        let arr: [u8; 32] = bytes.try_into().ok()?;
        let pk = iroh::PublicKey::from_bytes(&arr).ok()?;
        let info = self.inner.transport.ep.remote_info(pk).await?;
        let id = crate::base32_encode(info.id().as_bytes());
        let mut addrs = Vec::new();
        for a in info.addrs() {
            match a.addr() {
                iroh::TransportAddr::Ip(sock) => addrs.push(sock.to_string()),
                iroh::TransportAddr::Relay(url) => addrs.push(url.to_string()),
                other => addrs.push(format!("{:?}", other)),
            }
        }
        Some(NodeAddrInfo { id, addrs })
    }

    /// Per-peer connection statistics.
    ///
    /// Returns path information for each known transport address, including
    /// whether each path is via a relay or direct, and which is active.
    pub async fn peer_stats(&self, node_id_b32: &str) -> Option<PeerStats> {
        let bytes = crate::base32_decode(node_id_b32).ok()?;
        let arr: [u8; 32] = bytes.try_into().ok()?;
        let pk = iroh::PublicKey::from_bytes(&arr).ok()?;
        let info = self.inner.transport.ep.remote_info(pk).await?;

        let mut paths = Vec::new();
        let mut has_active_relay = false;
        let mut active_relay_url: Option<String> = None;

        for a in info.addrs() {
            let is_relay = a.addr().is_relay();
            let is_active = matches!(a.usage(), TransportAddrUsage::Active);

            let addr_str = match a.addr() {
                iroh::TransportAddr::Ip(sock) => sock.to_string(),
                iroh::TransportAddr::Relay(url) => {
                    if is_active {
                        has_active_relay = true;
                        active_relay_url = Some(url.to_string());
                    }
                    url.to_string()
                }
                other => format!("{:?}", other),
            };

            paths.push(PathInfo {
                relay: is_relay,
                addr: addr_str,
                active: is_active,
            });
        }

        // Enrich with QUIC connection-level stats if a pooled connection exists.
        let (rtt_ms, bytes_sent, bytes_received, lost_packets, sent_packets, congestion_window) =
            if let Some(pooled) = self.inner.http.pool.get_existing(pk, crate::ALPN).await {
                let s = pooled.conn.stats();
                let rtt = pooled.conn.rtt(iroh::endpoint::PathId::ZERO);
                (
                    rtt.map(|d| d.as_secs_f64() * 1000.0),
                    Some(s.udp_tx.bytes),
                    Some(s.udp_rx.bytes),
                    None,
                    None,
                    None,
                )
            } else {
                (None, None, None, None, None, None)
            };

        Some(PeerStats {
            relay: has_active_relay,
            relay_url: active_relay_url,
            paths,
            rtt_ms,
            bytes_sent,
            bytes_received,
            lost_packets,
            sent_packets,
            congestion_window,
        })
    }

    /// Subscribe to path changes for a specific peer.
    ///
    /// Spawns a background watcher task the first time a given peer is subscribed.
    /// The watcher polls `peer_stats()` every 200 ms and emits on the returned
    /// channel whenever the active path changes.
    ///
    /// Re-subscribing to a peer that already has a live watcher reuses that
    /// watcher: only the sender is swapped, so no additional task is spawned and
    /// `active_path_watchers` still counts exactly one live watcher per peer.
    pub fn subscribe_path_changes(
        &self,
        node_id_str: &str,
    ) -> tokio::sync::mpsc::UnboundedReceiver<PathInfo> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        // Replace any existing sender; the running watcher (if any) picks up the
        // new sender on its next poll. `insert` returns the previous sender when
        // a watcher is already live for this peer.
        let had_existing_watcher = self
            .inner
            .session
            .path_subs
            .insert(node_id_str.to_string(), tx)
            .is_some();

        // A watcher already exists for this peer — reuse it. Spawning another
        // would leak a task and over-count `active_path_watchers`.
        if had_existing_watcher {
            return rx;
        }

        let ep = self.clone();
        let nid = node_id_str.to_string();
        let event_tx = self.inner.session.event_tx.clone();
        self.inner
            .session
            .active_path_watchers
            .fetch_add(1, Ordering::Relaxed);

        tokio::spawn(async move {
            let mut last_key: Option<String> = None;
            let mut closed_rx = ep.inner.session.closed_rx.clone();
            loop {
                // Exit immediately if the endpoint has been closed.
                if *closed_rx.borrow() {
                    ep.inner.session.path_subs.remove(&nid);
                    break;
                }
                let is_closed = ep
                    .inner
                    .session
                    .path_subs
                    .get(&nid)
                    .map(|s| s.is_closed())
                    .unwrap_or(true);
                if is_closed {
                    ep.inner.session.path_subs.remove(&nid);
                    break;
                }

                if let Some(stats) = ep.peer_stats(&nid).await {
                    if let Some(active) = stats.paths.iter().find(|p| p.active) {
                        let key = format!("{}:{}", active.relay, active.addr);
                        if Some(&key) != last_key.as_ref() {
                            last_key = Some(key);
                            if let Some(sender) = ep.inner.session.path_subs.get(&nid) {
                                let _ = sender.send(active.clone());
                            }
                            let _ = event_tx.try_send(
                                crate::http::events::TransportEvent::path_change(
                                    &nid,
                                    &active.addr,
                                    active.relay,
                                ),
                            );
                        }
                    }
                }

                // Sleep 200 ms, but wake early if the endpoint is being closed.
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
                    result = closed_rx.wait_for(|v| *v) => {
                        let _ = result;
                        ep.inner.session.path_subs.remove(&nid);
                        break;
                    }
                }
            }
            ep.inner
                .session
                .active_path_watchers
                .fetch_sub(1, Ordering::Relaxed);
        });

        rx
    }

    /// Stop watching path changes for a specific peer.
    pub fn unsubscribe_path_changes(&self, node_id_str: &str) {
        self.inner.session.path_subs.remove(node_id_str);
    }
}

/// Pick the first routable `ip:port` from a set of already port-reconciled
/// direct addresses, formatted for advertisement.
///
/// The input addresses come from [`IrohEndpoint::direct_socket_addrs`], so
/// their ports are already the real bound QUIC port. This only applies the
/// routability filter. Kept in sync with `select_advertise_address` in
/// `iroh-http-discovery`; the two cannot share a single helper without
/// introducing a cross-crate dependency (both crates depend on `iroh`, neither
/// on the other).
fn select_dialable_direct(addrs: &[std::net::SocketAddr]) -> Option<String> {
    addrs
        .iter()
        .copied()
        .find(|a| is_routable_ip(&a.ip()))
        .map(|a| a.to_string())
}

/// Whether `ip` is routable off-link: not loopback, unspecified, or link-local.
///
/// A link-local address (IPv4 `169.254.0.0/16`, IPv6 `fe80::/10`) is only valid
/// on its own segment, so advertising it as this node's dialable address makes a
/// browsing peer's direct dial fail (#350). `Ipv6Addr::is_unicast_link_local` is
/// still unstable, so the `fe80::/10` prefix is matched by hand. Mirrors the
/// identical predicate in `iroh-http-discovery` and `iroh-http-tauri`.
fn is_routable_ip(ip: &std::net::IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    match ip {
        std::net::IpAddr::V4(v4) => !v4.is_link_local(),
        std::net::IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
    }
}

#[cfg(test)]
mod dialable_tests {
    use super::{is_routable_ip, select_dialable_direct};
    use std::net::SocketAddr;

    #[test]
    fn selects_first_routable_ip_with_reconciled_port() {
        // A reconciled address already carries the real bound QUIC port; the
        // selector just filters for routability and formats it.
        let addrs: Vec<SocketAddr> = vec![
            "127.0.0.1:59234".parse().unwrap(),
            "192.168.1.42:59234".parse().unwrap(),
        ];
        assert_eq!(
            select_dialable_direct(&addrs),
            Some("192.168.1.42:59234".to_string())
        );
    }

    #[test]
    fn skips_loopback_and_unspecified() {
        let addrs: Vec<SocketAddr> = vec![
            "127.0.0.1:59234".parse().unwrap(),
            "0.0.0.0:59234".parse().unwrap(),
        ];
        assert_eq!(select_dialable_direct(&addrs), None);
    }

    #[test]
    fn skips_link_local() {
        // A link-local address must not be advertised as dialable (#350);
        // prefer the real LAN address that follows it.
        let addrs: Vec<SocketAddr> = vec![
            "169.254.10.1:59234".parse().unwrap(),
            "10.0.0.5:59234".parse().unwrap(),
        ];
        assert_eq!(
            select_dialable_direct(&addrs),
            Some("10.0.0.5:59234".to_string())
        );
    }

    #[test]
    fn none_when_only_non_routable() {
        let addrs: Vec<SocketAddr> = vec!["169.254.10.1:59234".parse().unwrap()];
        assert_eq!(select_dialable_direct(&addrs), None);
    }

    #[test]
    fn brackets_ipv6() {
        let addrs: Vec<SocketAddr> = vec!["[2001:db8::1]:443".parse().unwrap()];
        assert_eq!(
            select_dialable_direct(&addrs),
            Some("[2001:db8::1]:443".to_string())
        );
    }

    #[test]
    fn routable_ip_predicate() {
        assert!(is_routable_ip(&"192.168.1.1".parse().unwrap()));
        assert!(is_routable_ip(&"2001:db8::1".parse().unwrap()));
        assert!(!is_routable_ip(&"127.0.0.1".parse().unwrap()));
        assert!(!is_routable_ip(&"0.0.0.0".parse().unwrap()));
        assert!(!is_routable_ip(&"169.254.1.1".parse().unwrap()));
        assert!(!is_routable_ip(&"fe80::1".parse().unwrap()));
    }
}
