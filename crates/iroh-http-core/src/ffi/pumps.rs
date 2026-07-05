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
use crate::{http::body::BoxError, CoreError};

// ── Shared pump helpers ───────────────────────────────────────────────────────

/// Default read buffer size for QUIC stream reads.
pub(crate) const PUMP_READ_BUF: usize = 64 * 1024;

/// True if `err` (or anything in its `source()` chain) is a
/// [`http_body_util::LengthLimitError`].
///
/// The tower-http `RequestBodyLimitLayer` wraps request bodies in
/// `http_body_util::Limited`, which yields a `LengthLimitError` once the
/// decoded byte cap is exceeded (see `http::server::stack::apply_body_limit`).
/// That case is a genuine over-limit condition and must map to
/// `ErrorCode::BodyTooLarge`; every other body-stream error (QUIC reset,
/// malformed framing, decompression failure) must not.
fn is_length_limit_error(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if e.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        cur = e.source();
    }
    false
}

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
    B::Error: Into<BoxError>,
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
/// so the caller can observe the overflow. The body reader also terminates
/// with `BodyTooLarge` instead of a clean EOF.
pub(crate) async fn pump_hyper_body_to_channel_limited<B>(
    body: B,
    writer: BodyWriter,
    max_bytes: Option<usize>,
    frame_timeout: std::time::Duration,
    mut overflow_tx: Option<tokio::sync::oneshot::Sender<()>>,
) where
    B: http_body::Body<Data = Bytes>,
    B::Error: Into<BoxError>,
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
                writer.abort(CoreError::timeout("body frame read timed out"));
                break;
            }
            Ok(None) => break,
            Ok(Some(r)) => r,
        };
        match frame_result {
            Err(e) => {
                // Box the error so we can inspect the concrete type behind the
                // body's `BoxError`. Deref (`&*boxed`) yields a trait object
                // carrying the *concrete* vtable, so `is::<LengthLimitError>`
                // and the `source()` walk see the real error type.
                let boxed: BoxError = e.into();
                if is_length_limit_error(&*boxed) {
                    // Stack-enforced decoded-body cap (tower-http Limited):
                    // a genuine over-limit condition.
                    tracing::warn!("iroh-http: request body exceeded configured limit: {boxed}");
                    writer.abort(CoreError::body_too_large(
                        "body exceeded configured size limit",
                    ));
                } else {
                    // Any other frame error (QUIC reset, malformed framing,
                    // decompression failure) is a stream/protocol error, not a
                    // size violation — do not mislabel it as BodyTooLarge.
                    tracing::warn!("iroh-http: body frame error: {boxed:?}");
                    writer.abort(CoreError::internal(format!("body stream error: {boxed}")));
                }
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
                            writer.abort(CoreError::body_too_large(format!(
                                "body exceeded configured size limit of {limit} bytes"
                            )));
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

#[cfg(test)]
mod tests {
    use super::super::handles::make_body_channel;
    use super::*;
    use http_body::Frame;
    use http_body_util::StreamBody;
    use std::convert::Infallible;
    use std::time::Duration;
    use tokio::sync::oneshot;

    /// A 50-byte chunk used to exercise the byte-limit boundary.
    const FIFTY: &[u8] = &[b'x'; 50];

    /// Build a body that yields each slice as one DATA frame, then EOF.
    fn body_from_chunks(
        chunks: Vec<&'static [u8]>,
    ) -> impl http_body::Body<Data = Bytes, Error = Infallible> {
        let frames = chunks
            .into_iter()
            .map(|c| Ok::<_, Infallible>(Frame::data(Bytes::from_static(c))));
        StreamBody::new(futures::stream::iter(frames))
    }

    /// Round-trip: every frame the body yields reaches the reader, in order.
    #[tokio::test]
    async fn pump_round_trip_forwards_all_chunks() {
        let (writer, reader) = make_body_channel();
        let body = body_from_chunks(vec![b"foo", b"bar", b"baz"]);
        let pump = tokio::spawn(pump_hyper_body_to_channel_limited(
            body,
            writer,
            None,
            Duration::from_secs(5),
            None,
        ));

        let mut collected = Vec::new();
        while let Some(chunk) = reader.next_chunk().await {
            collected.extend_from_slice(&chunk);
        }
        pump.await.unwrap();

        assert_eq!(collected, b"foobarbaz");
    }

    /// Regression (hardening batch / ISS-004): a body over `max_bytes` fires the
    /// overflow signal exactly once, forwards only the frames within the limit,
    /// and keeps draining the rest so the pump terminates (peer not stalled).
    #[tokio::test]
    async fn pump_over_limit_fires_overflow_and_drains_rest() {
        let (writer, reader) = make_body_channel();
        let (overflow_tx, overflow_rx) = oneshot::channel();
        // 4 × 50 = 200 bytes total, limit 100: frames 1–2 pass, 3 overflows, 4 drains.
        let body = body_from_chunks(vec![FIFTY, FIFTY, FIFTY, FIFTY]);
        let pump = tokio::spawn(pump_hyper_body_to_channel_limited(
            body,
            writer,
            Some(100),
            Duration::from_secs(5),
            Some(overflow_tx),
        ));

        let mut total = 0usize;
        while let Some(chunk) = reader.next_chunk().await {
            total += chunk.len();
        }
        pump.await.unwrap();

        // Only the two frames within the 100-byte limit reached the reader.
        assert_eq!(total, 100);
        // Overflow was signalled (sender not dropped without sending).
        assert!(
            overflow_rx.await.is_ok(),
            "overflow signal should fire once"
        );
    }

    /// Regression (hardening batch): a body that stalls mid-stream is cut off
    /// after `frame_timeout` instead of hanging the pump forever (slowloris).
    #[tokio::test]
    async fn pump_breaks_when_frame_read_times_out() {
        let (writer, reader) = make_body_channel();
        // One frame, then the body never yields again.
        use futures::StreamExt;
        let frames = futures::stream::iter(vec![Ok::<_, Infallible>(Frame::data(
            Bytes::from_static(b"first"),
        ))])
        .chain(futures::stream::pending());
        let body = StreamBody::new(frames);

        let start = std::time::Instant::now();
        let pump = tokio::spawn(pump_hyper_body_to_channel_limited(
            body,
            writer,
            None,
            Duration::from_millis(50),
            None,
        ));

        let mut collected = Vec::new();
        while let Some(chunk) = reader.next_chunk().await {
            collected.extend_from_slice(&chunk);
        }
        pump.await.unwrap();

        assert_eq!(collected, b"first");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "pump should cut off the stalled body promptly",
        );
    }

    /// Backpressure: with more frames than the channel capacity (32) and a slow
    /// reader, every frame is still delivered without loss.
    #[tokio::test]
    async fn pump_delivers_all_frames_under_slow_reader() {
        let (writer, reader) = make_body_channel();
        let body = body_from_chunks(vec![b"x".as_slice(); 50]);
        let pump = tokio::spawn(pump_hyper_body_to_channel_limited(
            body,
            writer,
            None,
            Duration::from_secs(5),
            None,
        ));

        let mut count = 0usize;
        while let Some(chunk) = reader.next_chunk().await {
            count += chunk.len();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        pump.await.unwrap();

        assert_eq!(count, 50);
    }

    /// Regression (#269/#271 review): a non-size body-stream error (e.g. QUIC
    /// reset, malformed framing) must surface as a generic `Internal` error,
    /// not be mislabelled as `BodyTooLarge`.
    #[tokio::test]
    async fn pump_non_length_error_surfaces_internal_not_body_too_large() {
        use http_body::Frame;
        let (writer, reader) = make_body_channel();
        let err = std::io::Error::other("connection reset by peer");
        let frames = futures::stream::iter(vec![
            Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(b"partial"))),
            Err(err),
        ]);
        let body = StreamBody::new(frames);
        let pump = tokio::spawn(pump_hyper_body_to_channel_limited(
            body,
            writer,
            None,
            Duration::from_secs(5),
            None,
        ));
        while reader.next_chunk().await.is_some() {}
        pump.await.unwrap();

        let code = reader
            .terminal_error
            .lock()
            .unwrap()
            .as_ref()
            .map(|e| e.code);
        assert_eq!(
            code,
            Some(crate::ErrorCode::Internal),
            "non-size frame errors must not be reported as BodyTooLarge"
        );
    }

    /// Regression (#269/#271 review): the stack-enforced decoded-body cap
    /// (`http_body_util::Limited`, wired by `RequestBodyLimitLayer`) surfaces a
    /// `LengthLimitError` on the generic `Err` branch, and that case *must*
    /// still map to `BodyTooLarge`. Built through `crate::Body` so the error
    /// type matches production (`BoxError`) exactly.
    #[tokio::test]
    async fn pump_length_limit_error_surfaces_body_too_large() {
        use http_body_util::Limited;
        let (writer, reader) = make_body_channel();
        // 150 bytes through a 100-byte Limited body → LengthLimitError.
        let inner = body_from_chunks(vec![FIFTY, FIFTY, FIFTY]);
        let body = crate::Body::new(Limited::new(inner, 100));
        let pump = tokio::spawn(pump_hyper_body_to_channel_limited(
            body,
            writer,
            None,
            Duration::from_secs(5),
            None,
        ));
        while reader.next_chunk().await.is_some() {}
        pump.await.unwrap();

        let code = reader
            .terminal_error
            .lock()
            .unwrap()
            .as_ref()
            .map(|e| e.code);
        assert_eq!(
            code,
            Some(crate::ErrorCode::BodyTooLarge),
            "stack-enforced decoded cap (LengthLimitError) must map to BodyTooLarge"
        );
    }

    /// Cancellation: once the reader is dropped, the pump stops promptly instead
    /// of blocking on a full channel.
    #[tokio::test]
    async fn pump_stops_when_reader_dropped() {
        let (writer, reader) = make_body_channel();
        drop(reader);
        let body = body_from_chunks(vec![b"a", b"b", b"c"]);

        let res = tokio::time::timeout(
            Duration::from_secs(2),
            pump_hyper_body_to_channel_limited(body, writer, None, Duration::from_secs(5), None),
        )
        .await;

        assert!(
            res.is_ok(),
            "pump should terminate after the reader is dropped"
        );
    }
}
