//! `Session` — a QUIC connection to a single remote peer.
//!
//! [`Session::connect`] establishes (or accepts via [`Session::accept`]) a QUIC
//! connection and returns a session value backed by an opaque handle.
//! Bidirectional streams, unidirectional streams, and datagrams are all
//! accessible through methods on [`Session`].
//!
//! The shape mirrors W3C WebTransport: every method has a direct counterpart
//! on the `WebTransport` interface.
//!
// Legitimate FFI wiring — uses the disallowed types intentionally.
#![allow(clippy::disallowed_types)]
//! Lives under `mod ffi` (Slice E, #187) because `Session` is `u64`-handle-
//! shaped: it wraps a slotmap entry in [`crate::ffi::handles::HandleStore`]
//! and returns [`crate::FfiDuplexStream`]. A pure-Rust QUIC session API would
//! sit under `mod http` and yield typed stream values directly; that is not
//! today's API. Until it is, this module belongs on the FFI side.

use iroh::endpoint::Connection;
use serde::Serialize;

use crate::{
    ffi::handles::{HandleStore, SessionEntry},
    ffi::pumps::{pump_body_to_quic_send, pump_quic_recv_to_body},
    parse_node_addr, CoreError, FfiDuplexStream, IrohEndpoint, ALPN_DUPLEX,
};

/// Returns `true` if the connection error means "connection ended" rather
/// than a protocol-level bug.  Used to return `None` instead of `Err`.
fn is_connection_closed(err: &iroh::endpoint::ConnectionError) -> bool {
    use iroh::endpoint::ConnectionError::*;
    matches!(
        err,
        ApplicationClosed(_) | ConnectionClosed(_) | Reset | TimedOut | LocallyClosed
    )
}

/// Close information returned when a session ends.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseInfo {
    pub close_code: u64,
    pub reason: String,
}

/// A QUIC session to a single remote peer.
///
/// Cheap to clone — this is just an [`IrohEndpoint`] ref-clone plus a `u64`
/// handle into the endpoint's session registry. Sessions are not pooled;
/// closing one session has no effect on other sessions to the same peer.
#[derive(Clone)]
pub struct Session {
    endpoint: IrohEndpoint,
    handle: u64,
}

impl Session {
    /// Reconstruct a `Session` from a handle previously obtained via
    /// [`Session::connect`] or [`Session::accept`].
    ///
    /// FFI adapters use this to rehydrate a session from a `u64` handle that
    /// crossed a language boundary.
    pub fn from_handle(endpoint: IrohEndpoint, handle: u64) -> Self {
        Self { endpoint, handle }
    }

    /// The opaque handle identifying this session inside the endpoint registry.
    pub fn handle(&self) -> u64 {
        self.handle
    }

    /// Establish a session (QUIC connection) to a remote peer.
    ///
    /// Each call creates a **dedicated** QUIC connection — sessions are not
    /// pooled. Closing one session handle cannot affect other sessions to the
    /// same peer. (Fetch operations continue to use the shared pool for
    /// efficiency; sessions opt out because [`Session::close`] closes the
    /// underlying connection.)
    pub async fn connect(
        endpoint: IrohEndpoint,
        remote_node_id: &str,
        direct_addrs: Option<&[std::net::SocketAddr]>,
    ) -> Result<Self, CoreError> {
        let parsed = parse_node_addr(remote_node_id)?;
        let node_id = parsed.node_id;
        let mut addr = iroh::EndpointAddr::new(node_id);
        for relay in parsed.relay_urls {
            addr = addr.with_relay_url(relay);
        }
        for a in &parsed.direct_addrs {
            addr = addr.with_ip_addr(*a);
        }
        if let Some(addrs) = direct_addrs {
            for a in addrs {
                addr = addr.with_ip_addr(*a);
            }
        }

        let conn = endpoint
            .raw()
            .connect(addr, ALPN_DUPLEX)
            .await
            .map_err(|e| CoreError::connection_failed(format!("connect session: {e}")))?;

        let handle = endpoint.handles().insert_session(SessionEntry { conn })?;

        Ok(Self { endpoint, handle })
    }

    /// Accept an incoming session (QUIC connection) from a remote peer.
    ///
    /// Blocks until a peer connects.  Returns the new session, or `None` if
    /// the endpoint is shutting down.
    pub async fn accept(endpoint: IrohEndpoint) -> Result<Option<Self>, CoreError> {
        let incoming = match endpoint.raw().accept().await {
            Some(inc) => inc,
            None => return Ok(None),
        };

        let conn = incoming
            .await
            .map_err(|e| CoreError::connection_failed(format!("accept session: {e}")))?;

        // Enforce the negotiated ALPN: only accept connections that negotiated
        // the duplex session protocol. A connection negotiated for plain HTTP
        // must not be surfaced as a raw session (protocol confusion).
        let alpn = conn.alpn();
        if alpn != ALPN_DUPLEX {
            let detail = format!(
                "accept session: unexpected ALPN {:?}",
                String::from_utf8_lossy(alpn)
            );
            conn.close(0u32.into(), b"unexpected ALPN");
            return Err(CoreError::connection_failed(detail));
        }

        let handle = endpoint.handles().insert_session(SessionEntry { conn })?;

        Ok(Some(Self { endpoint, handle }))
    }

    fn conn(&self) -> Result<Connection, CoreError> {
        self.endpoint
            .handles()
            .lookup_session(self.handle)
            .map(|s| s.conn.clone())
            .ok_or_else(|| CoreError::invalid_handle(self.handle))
    }

    /// Return the remote peer's public key.
    pub fn remote_id(&self) -> Result<iroh::PublicKey, CoreError> {
        self.conn().map(|c| c.remote_id())
    }

    /// Open a new bidirectional stream on this session.
    ///
    /// Returns [`FfiDuplexStream`] with `read_handle` / `write_handle` backed by
    /// body channels — the same interface used by fetch and raw_connect.
    pub async fn create_bidi_stream(&self) -> Result<FfiDuplexStream, CoreError> {
        let conn = self.conn()?;

        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| CoreError::connection_failed(format!("open_bi: {e}")))?;

        wrap_bidi_stream(self.endpoint.handles(), send, recv)
    }

    /// Accept the next incoming bidirectional stream from the remote side.
    ///
    /// Blocks until the remote opens a stream, or returns `None` when the
    /// connection is closed.
    pub async fn next_bidi_stream(&self) -> Result<Option<FfiDuplexStream>, CoreError> {
        let conn = self.conn()?;

        match conn.accept_bi().await {
            Ok((send, recv)) => Ok(Some(wrap_bidi_stream(self.endpoint.handles(), send, recv)?)),
            Err(e) if is_connection_closed(&e) => Ok(None),
            Err(e) => Err(CoreError::connection_failed(format!("accept_bi: {e}"))),
        }
    }

    /// Close this session's QUIC connection.
    ///
    /// The session handle is **not** removed here — [`closed`](Self::closed)
    /// does the cleanup after `conn.closed()` resolves.  This avoids a race
    /// where `close()` removes the handle before a concurrently-running
    /// `closed()` future can look it up.
    ///
    /// `close_code` is an application-level error code (maps to QUIC VarInt).
    /// `reason` is a human-readable string sent to the peer.
    pub fn close(&self, close_code: u64, reason: &str) -> Result<(), CoreError> {
        let conn = self.conn()?;
        let code = iroh::endpoint::VarInt::from_u64(close_code).map_err(|_| {
            CoreError::invalid_input(format!(
                "close_code {close_code} exceeds QUIC VarInt max (2^62 - 1)"
            ))
        })?;
        conn.close(code, reason.as_bytes());
        Ok(())
    }

    /// Wait for the QUIC handshake to complete on this session.
    ///
    /// Resolves immediately if the handshake has already completed.
    pub async fn ready(&self) -> Result<(), CoreError> {
        // Validate handle exists — keeps error behavior consistent with other session APIs.
        let _conn = self.conn()?;
        // iroh connections are fully established by the time Session::connect returns,
        // so ready always resolves immediately. Kept for WebTransport API compatibility.
        Ok(())
    }

    /// Wait for the session to close and return the close information.
    ///
    /// Blocks until the connection is closed by either side.  Removes the
    /// session from the registry so resources are freed.
    ///
    /// If the handle has already been removed (e.g. by a concurrent
    /// `closed()` call), returns a default `CloseInfo` instead of an error.
    pub async fn closed(&self) -> Result<CloseInfo, CoreError> {
        let conn = match self.conn() {
            Ok(c) => c,
            Err(_) => {
                // Handle already removed — another `closed()` or endpoint
                // shutdown beat us to it.
                return Ok(CloseInfo {
                    close_code: 0,
                    reason: String::new(),
                });
            }
        };
        let err = conn.closed().await;
        // Connection is dead — clean up the registry entry.
        self.endpoint.handles().remove_session(self.handle);
        let (close_code, reason) = parse_connection_error(&err);
        Ok(CloseInfo { close_code, reason })
    }

    /// Open a new unidirectional (send-only) stream on this session.
    ///
    /// Returns a write handle backed by a body channel.
    pub async fn create_uni_stream(&self) -> Result<u64, CoreError> {
        let conn = self.conn()?;
        let send = conn
            .open_uni()
            .await
            .map_err(|e| CoreError::connection_failed(format!("open_uni: {e}")))?;

        let handles = self.endpoint.handles();
        let (send_writer, send_reader) = handles.make_body_channel();
        let write_handle = handles.insert_writer(send_writer)?;
        tokio::spawn(pump_body_to_quic_send(send_reader, send));

        Ok(write_handle)
    }

    /// Accept the next incoming unidirectional (receive-only) stream.
    ///
    /// Returns a read handle, or `None` when the connection is closed.
    pub async fn next_uni_stream(&self) -> Result<Option<u64>, CoreError> {
        let conn = self.conn()?;

        match conn.accept_uni().await {
            Ok(recv) => {
                let handles = self.endpoint.handles();
                let (recv_writer, recv_reader) = handles.make_body_channel();
                let read_handle = handles.insert_reader(recv_reader)?;
                tokio::spawn(pump_quic_recv_to_body(recv, recv_writer));
                Ok(Some(read_handle))
            }
            Err(e) if is_connection_closed(&e) => Ok(None),
            Err(e) => Err(CoreError::connection_failed(format!("accept_uni: {e}"))),
        }
    }

    /// Send a datagram on this session.
    ///
    /// Fails if `data.len()` exceeds [`Session::max_datagram_size`].
    pub fn send_datagram(&self, data: &[u8]) -> Result<(), CoreError> {
        let conn = self.conn()?;
        conn.send_datagram(bytes::Bytes::copy_from_slice(data))
            .map_err(|e| match e {
                iroh::endpoint::SendDatagramError::TooLarge => CoreError::body_too_large(
                    "datagram exceeds path MTU; check Session::max_datagram_size()",
                ),
                _ => CoreError::internal(format!("send_datagram: {e}")),
            })
    }

    /// Receive the next datagram from this session.
    ///
    /// Blocks until a datagram arrives, or returns `None` when the connection closes.
    pub async fn recv_datagram(&self) -> Result<Option<Vec<u8>>, CoreError> {
        let conn = self.conn()?;
        match conn.read_datagram().await {
            Ok(data) => Ok(Some(data.to_vec())),
            Err(e) if is_connection_closed(&e) => Ok(None),
            Err(e) => Err(CoreError::connection_failed(format!("recv_datagram: {e}"))),
        }
    }

    /// Return the current maximum datagram payload size for this session.
    ///
    /// Returns `None` if datagrams are not supported on the current path.
    pub fn max_datagram_size(&self) -> Result<Option<usize>, CoreError> {
        let conn = self.conn()?;
        Ok(conn.max_datagram_size())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Wrap raw QUIC send/recv streams into body-channel–backed `FfiDuplexStream`.
fn wrap_bidi_stream(
    handles: &HandleStore,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
) -> Result<FfiDuplexStream, CoreError> {
    let mut guard = handles.insert_guard();

    // Receive side: pump from QUIC recv → BodyWriter → BodyReader (JS reads via nextChunk).
    let (recv_writer, recv_reader) = handles.make_body_channel();
    let read_handle = guard.insert_reader(recv_reader)?;
    tokio::spawn(pump_quic_recv_to_body(recv, recv_writer));

    // Send side: pump from BodyReader (JS writes via sendChunk) → QUIC send.
    let (send_writer, send_reader) = handles.make_body_channel();
    let write_handle = guard.insert_writer(send_writer)?;
    tokio::spawn(pump_body_to_quic_send(send_reader, send));

    guard.commit();
    Ok(FfiDuplexStream {
        read_handle,
        write_handle,
    })
}

/// Extract close code and reason from a QUIC `ConnectionError`.
fn parse_connection_error(err: &iroh::endpoint::ConnectionError) -> (u64, String) {
    match err {
        iroh::endpoint::ConnectionError::ApplicationClosed(info) => {
            let code: u64 = info.error_code.into();
            let reason = String::from_utf8_lossy(&info.reason).into_owned();
            (code, reason)
        }
        other => (0, other.to_string()),
    }
}
