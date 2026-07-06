//! Endpoint observability types — snapshots, events, peer statistics.

use serde::{Deserialize, Serialize};

/// Serialisable node address: node ID + relay and direct addresses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAddrInfo {
    /// Base32-encoded public key.
    pub id: String,
    /// Relay URLs and/or `ip:port` direct addresses.
    pub addrs: Vec<String>,
}

/// Endpoint-level observability snapshot.
///
/// Returned by [`super::IrohEndpoint::endpoint_stats`].  All counts are
/// point-in-time reads and may change between calls.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EndpointStats {
    /// Number of currently open body reader handles.
    pub active_readers: usize,
    /// Number of currently open body writer handles.
    pub active_writers: usize,
    /// Number of live QUIC sessions (WebTransport connections).
    pub active_sessions: usize,
    /// Total number of allocated (reader + writer + session + other) handles.
    pub total_handles: usize,
    /// Number of QUIC connections currently cached in the connection pool.
    pub pool_size: usize,
    /// Number of live QUIC connections accepted by the serve loop.
    pub active_connections: usize,
    /// Number of HTTP requests currently being processed.
    pub active_requests: usize,
    /// Number of live path-change subscriptions.
    pub active_path_subscriptions: usize,
    /// Number of live path-change watcher tasks.
    pub active_path_watchers: usize,
}

/// A connection lifecycle event fired when a QUIC peer connection opens or closes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionEvent {
    /// Base32-encoded public key of the peer.
    pub peer_id: String,
    /// `true` when this is the first connection from the peer (0→1), `false` when the last one closes (1→0).
    pub connected: bool,
}

/// Per-peer connection statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerStats {
    /// Whether the peer is connected via a relay server (vs direct).
    pub relay: bool,
    /// Active relay URL, if any.
    pub relay_url: Option<String>,
    /// All known paths to this peer.
    pub paths: Vec<PathInfo>,
    /// Round-trip time in milliseconds.  `None` if no active QUIC connection is pooled.
    pub rtt_ms: Option<f64>,
    /// Total UDP bytes sent to this peer.  `None` if no active QUIC connection is pooled.
    pub bytes_sent: Option<u64>,
    /// Total UDP bytes received from this peer.  `None` if no active QUIC connection is pooled.
    pub bytes_received: Option<u64>,
    /// Total packets lost on the QUIC path.  `None` if no active QUIC connection is pooled.
    pub lost_packets: Option<u64>,
    /// Total packets sent on the QUIC path.  `None` if no active QUIC connection is pooled.
    pub sent_packets: Option<u64>,
    /// Current congestion window in bytes.  `None` if no active QUIC connection is pooled.
    pub congestion_window: Option<u64>,
}

/// Network path information for a single transport address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathInfo {
    /// Whether this path goes through a relay server.
    pub relay: bool,
    /// The relay URL (if relay) or `ip:port` (if direct).
    pub addr: String,
    /// Whether this is the currently selected/active path.
    pub active: bool,
}
