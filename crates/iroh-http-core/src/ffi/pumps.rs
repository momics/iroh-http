//! Shared pump helpers bridging QUIC streams and body channels.
//!
//! Used by the FFI session API (uni/bidi streams) and by the FFI fetch
//! wrapper, which drains a `hyper::Response` body into a slotmap
//! [`BodyWriter`].
//!
//! These helpers are FFI plumbing — they translate streaming bodies into
//! the channel-backed handle world that the JS adapters consume. The
//! pure-Rust `http::client::fetch_request` does not need them; it returns a
//! [`hyper::Response<Body>`] and lets the caller consume the body
// Legitimate FFI wiring — uses the disallowed types intentionally.
#![allow(clippy::disallowed_types)]
//! directly. Slice D (#186) moved them out of `http/client.rs` to remove
//! the last `crate::ffi` import from `mod http`.

use bytes::Bytes;

use super::handles::{BodyReader, BodyWriter};

// ── Shared pump helpers ───────────────────────────────────────────────────────

/// Default read buffer size for QUIC stream reads.
pub(crate) const PUMP_READ_BUF: usize = 64 * 1024;

/// Pump raw bytes from a QUIC `RecvStream` into a `BodyWriter`.
///
/// Reads `PUMP_READ_BUF`-sized chunks and forwards them through the body
/// channel.  Stops when the stream ends or the writer is dropped.
///
/// Kept as an imperative loop, not a `Sink`-based `forward()`. A
/// stream-adapter prototype was evaluated and rejected in #174 — see the
/// closing comment for the LoC analysis. `iroh::endpoint::RecvStream`
/// has no built-in `Stream<Item = Result<Bytes, _>>` shape, so wrapping
/// it costs more code than this loop.
pub(crate) async fn pump_quic_recv_to_body(
    mut recv: iroh::endpoint::RecvStream,
    writer: BodyWriter,
) {
    while let Ok(Some(chunk)) = recv.read_chunk(PUMP_READ_BUF).await {
        if writer.send_chunk(chunk).await.is_err() {
            break;
        }
    }
    // writer drops → BodyReader sees EOF.
}

/// Pump raw bytes from a `BodyReader` into a QUIC `SendStream`.
///
/// Reads chunks from the body channel and writes them to the stream.
/// Finishes the stream when the reader reaches EOF.
///
/// Kept as an imperative loop, not a `Sink`-based `forward()`. A
/// `QuicSendSink` wrapper was prototyped and rejected in #174 — see the
/// closing comment for the LoC analysis. `iroh::endpoint::SendStream`
/// is `&mut`-bound and stream-sequential, so a `Sink<Bytes>` impl has
/// to invent the shape (Option-slot for ownership shuffling + boxed
/// in-flight `write_all`); that wrapper is bigger than this loop.
pub(crate) async fn pump_body_to_quic_send(
    reader: BodyReader,
    mut send: iroh::endpoint::SendStream,
) {
    loop {
        match reader.next_chunk().await {
            None => break,
            Some(data) => {
                if send.write_all(&data).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = send.finish();
}

// ── Hyper body → BodyWriter pumps ─────────────────────────────────────────────
//
// Slice E (#187): `pump_hyper_body_to_channel{,_limited}` are kept
// intentionally. They are the FFI contract — JS adapters consume the
// response/request body via a slotmap `body_handle`, so the hyper body
// has to land in a `BodyWriter`-backed channel before JS can read it.
// The reverse direction (channel → hyper) does not need a pump because
// `BodyReader: http_body::Body` (commit `6fb9c1b`) — `Body::new(reader)`
// flows directly into hyper. See [`crate::ffi::fetch::package_response`]
// and [`crate::ffi::dispatcher`] for the only call sites; both are
// inside `mod ffi` and remain there per the epic-#182 layering rule.

/// Drain a hyper body into `BodyWriter`.
/// Generic over any body type with `Data = Bytes` (e.g. `Incoming`, `DecompressionBody`).
#[allow(dead_code)]
pub(crate) async fn pump_hyper_body_to_channel<B>(body: B, writer: BodyWriter)
where
    B: http_body::Body<Data = Bytes>,
    B::Error: std::fmt::Debug,
{
    let timeout = writer.drain_timeout;
    pump_hyper_body_to_channel_limited(body, writer, None, timeout, None).await;
}

/// Drain with optional byte limit and a per-frame read timeout.
///
/// `frame_timeout` bounds how long we wait for each individual body frame.
/// A slow-drip peer that stalls indefinitely will be cut off after this deadline.
///
/// When a byte limit is set and the body exceeds it, `overflow_tx` is fired
/// so the caller can return a `413 Content Too Large` response (ISS-004).
pub(crate) async fn pump_hyper_body_to_channel_limited<B>(
    body: B,
    writer: BodyWriter,
    max_bytes: Option<usize>,
    frame_timeout: std::time::Duration,
    mut overflow_tx: Option<tokio::sync::oneshot::Sender<()>>,
) where
    B: http_body::Body<Data = Bytes>,
    B::Error: std::fmt::Debug,
{
    use http_body_util::BodyExt;
    // Box::pin gives Pin<Box<B>>: Unpin (Box<T>: Unpin ∀T), which satisfies BodyExt::frame().
    let mut body = Box::pin(body);
    let mut total = 0usize;
    // Set to true once max_bytes is exceeded. When true, frames are read and
    // discarded so the peer's QUIC send stream can receive flow-control ACKs
    // and close cleanly instead of stalling until idle timeout (ISS-015).
    let mut overflowed = false;

    loop {
        let frame_result = match tokio::time::timeout(frame_timeout, body.frame()).await {
            Err(_elapsed) => {
                tracing::warn!("iroh-http: body frame read timed out after {frame_timeout:?}");
                break;
            }
            Ok(None) => break,
            Ok(Some(r)) => r,
        };
        match frame_result {
            Err(e) => {
                tracing::warn!("iroh-http: body frame error: {e:?}");
                break;
            }
            Ok(frame) => {
                if overflowed {
                    // Drain: discard the frame but keep reading so the QUIC
                    // flow-control window advances and the peer can finish
                    // writing its body.
                    continue;
                }
                if frame.is_data() {
                    let data = frame.into_data().expect("is_data checked above");
                    total = total.saturating_add(data.len());
                    if let Some(limit) = max_bytes {
                        if total > limit {
                            tracing::warn!("iroh-http: request body exceeded {limit} bytes");
                            // ISS-004: signal overflow so the serve path can send 413.
                            if let Some(tx) = overflow_tx.take() {
                                let _ = tx.send(());
                            }
                            overflowed = true;
                            continue; // drain remaining frames without stalling the peer
                        }
                    }
                    if writer.send_chunk(data).await.is_err() {
                        return; // reader dropped
                    }
                }
            }
        }
    }

    drop(writer);
}
