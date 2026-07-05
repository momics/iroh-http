//! C-ABI entry point for the Deno FFI adapter.
//!
//! Most dispatch goes through the JSON `iroh_http_call` symbol.  Hot-path
//! streaming operations use dedicated binary symbols to avoid base64 overhead:
//! - `iroh_http_next_chunk` — read a body chunk directly into a caller buffer
//! - `iroh_http_send_chunk` — write a body chunk directly from a caller buffer
//!
//! Request/response critical-path operations use synchronous symbols so they
//! execute inline on the JS thread (like Node's `#[napi]`), avoiding contention
//! with long-blocking ops on Deno's single-threaded tokio runtime (#122):
//! - `iroh_http_respond`      — send response head for a pending request
//! - `iroh_http_finish_body`  — signal body-complete (drop writer)
//! - `iroh_http_cancel_reader`— cancel a body reader
//!
//! All async symbols are `nonblocking: true` in the Deno `dlopen` call.
//! Sync symbols are `nonblocking: false`.

mod dispatch;
mod serve_registry;

use iroh_http_core::registry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Global multi-threaded Tokio runtime.  Initialised once on the first FFI call.
/// Returns `None` if the OS could not create the required threads.
pub(crate) fn runtime() -> Option<&'static tokio::runtime::Runtime> {
    static RT: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .ok()
    })
    .as_ref()
}

// ── Split-fetch registry (#126) ───────────────────────────────────────────────
//
// `iroh_http_start_fetch` spawns the async fetch on the tokio runtime and
// stores the `JoinHandle` in this registry.  `iroh_http_poll_fetch` checks
// whether the task has completed.  Both are `nonblocking: false` (sync on
// JS thread), eliminating the ~100–200µs spawn_blocking scheduling overhead
// that `iroh_http_call` incurs on every async FFI call.

static FETCH_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Result of a completed async fetch — either JSON bytes or an error.
type FetchResult = Result<Vec<u8>, Vec<u8>>;

fn fetch_registry() -> &'static Mutex<HashMap<u64, tokio::task::JoinHandle<FetchResult>>> {
    static R: OnceLock<Mutex<HashMap<u64, tokio::task::JoinHandle<FetchResult>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Completed fetch results waiting to be picked up by poll_fetch.
fn fetch_results() -> &'static Mutex<HashMap<u64, FetchResult>> {
    static R: OnceLock<Mutex<HashMap<u64, FetchResult>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── Stream error cache ────────────────────────────────────────────────────────

type StreamErrorKey = (u64, u64);
type StreamErrorMap = HashMap<StreamErrorKey, Vec<u8>>;

fn stream_errors() -> &'static Mutex<StreamErrorMap> {
    static R: OnceLock<Mutex<StreamErrorMap>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_stream_error(endpoint_handle: u64, handle: u64, error: iroh_http_core::CoreError) {
    stream_errors()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            (endpoint_handle, handle),
            iroh_http_adapter::core_error_to_json(&error).into_bytes(),
        );
}

// ── Overflow response cache ───────────────────────────────────────────────────
//
// When dispatch produces a response larger than the caller-provided output
// buffer, we cache the encoded bytes under a monotonic token and return the
// token to the caller (written into the first 8 bytes of the output buffer).
// The caller retries with method `"__cached"` and the 8-byte token as payload,
// avoiding a second dispatch of the original method.

static OVERFLOW_COUNTER: AtomicU64 = AtomicU64::new(1);

/// ISS-014: maximum number of cached overflow entries to prevent unbounded growth.
const OVERFLOW_MAX_ENTRIES: usize = 256;
/// ISS-014: maximum total bytes across all cached entries.
const OVERFLOW_MAX_BYTES: usize = 64 * 1024 * 1024; // 64 MB

/// Overflow entry with insertion timestamp for TTL eviction.
struct OverflowEntry {
    data: Vec<u8>,
    created: std::time::Instant,
}

/// TTL for overflow cache entries.
const OVERFLOW_TTL: std::time::Duration = std::time::Duration::from_secs(30);

fn overflow_cache() -> &'static Mutex<HashMap<u64, OverflowEntry>> {
    static C: OnceLock<Mutex<HashMap<u64, OverflowEntry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Evict expired and over-budget entries from the overflow cache.
fn evict_overflow(cache: &mut HashMap<u64, OverflowEntry>) {
    // Remove expired entries first.
    cache.retain(|_, e| e.created.elapsed() < OVERFLOW_TTL);
    // If still over budget, remove oldest entries until within limits.
    while cache.len() > OVERFLOW_MAX_ENTRIES
        || cache.values().map(|e| e.data.len()).sum::<usize>() > OVERFLOW_MAX_BYTES
    {
        if let Some((&oldest_key, _)) = cache.iter().min_by_key(|(_, e)| e.created) {
            cache.remove(&oldest_key);
        } else {
            break;
        }
    }
}

/// Single-dispatch FFI entry point.
///
/// Parameters:
/// - `method_ptr` / `method_len` — UTF-8 encoded method name.
/// - `payload_ptr` / `payload_len` — JSON-encoded payload bytes.
/// - `out_ptr` / `out_cap` — caller-allocated output buffer.
///
/// Return value:
/// - `>= 0` — number of bytes written to `out_ptr`.
/// - `< 0`  — `-(required_size)`; caller must retry with a larger buffer.
///
/// The output buffer always contains a JSON object of the form
/// `{"ok": <value>}` on success or `{"err": "<message>"}` on failure.
///
/// This symbol is declared `nonblocking: true` in the Deno `dlopen` call, so
/// it is invoked on the Deno thread pool and returns a `Promise<i32>`.
///
/// # Safety
/// `method_ptr`, `payload_ptr`, and `out_ptr` must be valid for the lengths
/// provided and must not overlap. Null pointers are only valid when the
/// corresponding length is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iroh_http_call(
    method_ptr: *const u8,
    method_len: usize,
    payload_ptr: *const u8,
    payload_len: usize,
    out_ptr: *mut u8,
    out_cap: usize,
) -> i32 {
    // Validate all pointer/length combinations at the FFI boundary.
    if method_len > 0 && method_ptr.is_null() {
        return -1;
    }
    if payload_len > 0 && payload_ptr.is_null() {
        return -1;
    }
    if out_cap > 0 && out_ptr.is_null() {
        return -1;
    }

    // SAFETY: Guards above ensure pointers are non-null when len > 0. Use empty
    // slices for zero-length inputs to avoid passing null to `from_raw_parts`,
    // which is UB even when len is 0 (SEC-001).
    let method_bytes: &[u8] = if method_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(method_ptr, method_len) }
    };
    let method = std::str::from_utf8(method_bytes).unwrap_or("__invalid_utf8__");
    let payload: &[u8] = if payload_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) }
    };

    // ── Cached-response retrieval (overflow retry path) ───────────────────
    if method == "__cached" {
        if payload_len >= 8 {
            let token = u64::from_le_bytes(
                payload[0..8]
                    .try_into()
                    .expect("payload_len >= 8 already checked"),
            );
            if let Some(entry) = overflow_cache()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&token)
            {
                if entry.data.len() <= out_cap {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            entry.data.as_ptr(),
                            out_ptr,
                            entry.data.len(),
                        );
                    }
                    return entry.data.len() as i32;
                }
                // Buffer still too small — put it back (shouldn't happen).
                let len = entry.data.len();
                overflow_cache()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(token, entry);
                return i32::try_from(len)
                    .map(|n| 0i32.wrapping_sub(n))
                    .unwrap_or(i32::MIN);
            }
        }
        return -1;
    }

    // ── Normal dispatch ───────────────────────────────────────────────────
    let rt = match runtime() {
        Some(rt) => rt,
        None => {
            let err =
                br#"{"err":"RUNTIME_INIT_FAILED: OS refused to create threads for Tokio runtime"}"#;
            if err.len() <= out_cap {
                unsafe { std::ptr::copy_nonoverlapping(err.as_ptr(), out_ptr, err.len()) };
                return err.len() as i32;
            }
            return i32::try_from(err.len())
                .map(|n| 0i32.wrapping_sub(n))
                .unwrap_or(i32::MIN);
        }
    };
    let response = rt.block_on(dispatch::dispatch(method, payload));

    let encoded = serde_json::to_vec(&response).unwrap_or_else(|e| {
        serde_json::to_vec(&serde_json::json!({ "err": e.to_string() }))
            .expect("static error JSON is always valid")
    });

    let len = encoded.len();
    if len > out_cap {
        // Cache the response and write a retrieval token into the output
        // buffer so the caller can retry without re-dispatching.
        let token = OVERFLOW_COUNTER.fetch_add(1, Ordering::Relaxed);
        if out_cap >= 8 {
            let token_bytes = token.to_le_bytes();
            unsafe {
                std::ptr::copy_nonoverlapping(token_bytes.as_ptr(), out_ptr, 8);
            }
        }
        let mut cache = overflow_cache().lock().unwrap_or_else(|e| e.into_inner());
        // ISS-014: evict before inserting to enforce size/time bounds.
        evict_overflow(&mut cache);
        cache.insert(
            token,
            OverflowEntry {
                data: encoded,
                created: std::time::Instant::now(),
            },
        );
        return i32::try_from(len)
            .map(|n| 0i32.wrapping_sub(n))
            .unwrap_or(i32::MIN);
    }
    // SAFETY: `out_ptr` is non-null (checked above) and `out_cap >= len`.
    unsafe {
        std::ptr::copy_nonoverlapping(encoded.as_ptr(), out_ptr, len);
    }
    len as i32
}

/// Raw-buffer `nextChunk` — bypasses JSON dispatch for streaming throughput.
///
/// Writes the next chunk bytes directly into `out_ptr[0..out_cap]`.
///
/// Return value:
/// - `n > 0`  — bytes written into the buffer.
/// - `n == 0` — end of stream; no more chunks.
/// - `n < 0`  — `|n|` bytes required; caller must retry with a larger buffer.
///
/// This symbol is declared `nonblocking: true` in the Deno `dlopen` call.
///
/// # Safety
/// `out_ptr` must be valid for `out_cap` bytes and must not alias any other
/// active reference for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iroh_http_next_chunk(
    endpoint_handle: u64,
    handle: u64,
    out_ptr: *mut u8,
    out_cap: usize,
) -> i32 {
    if out_cap > 0 && out_ptr.is_null() {
        return -1;
    }

    let ep = match registry::get_endpoint(endpoint_handle) {
        Some(ep) => ep,
        None => return -1,
    };

    let result = match runtime() {
        Some(rt) => rt.block_on(ep.handles().next_chunk(handle)),
        None => return -1,
    };

    match result {
        Err(e) => {
            record_stream_error(endpoint_handle, handle, e);
            -1
        }
        Ok(None) => 0,
        Ok(Some(b)) => {
            let len = b.len();
            if len > out_cap {
                return i32::try_from(len)
                    .map(|n| 0i32.wrapping_sub(n))
                    .unwrap_or(i32::MIN);
            }
            // SAFETY: caller guarantees out_ptr is valid for out_cap bytes,
            // and we have verified len <= out_cap.
            unsafe {
                std::ptr::copy_nonoverlapping(b.as_ptr(), out_ptr, len);
            }
            len as i32
        }
    }
}

/// Synchronous non-blocking `tryNextChunk` — attempt to read a body chunk
/// without entering the `spawn_blocking` thread pool.
///
/// #126: By running on the JS thread (`nonblocking: false`), this skips the
/// ~100–200µs `spawn_blocking` scheduling overhead per call.  When data is
/// already buffered in the channel (typical for small responses that arrive
/// before JS processes the rawFetch result), this returns it instantly.
///
/// Return value:
/// - `n > 0`  — `n` bytes written into the output buffer.
/// - `0`      — end of stream (EOF).
/// - `-1`     — hard error (endpoint gone, handle invalid).
/// - `-2`     — no data available yet; fall back to async `nextChunk`.
/// - `n < -2` — `|n|` bytes required; retry with a larger buffer.
///
/// # Safety
/// `out_ptr` must be valid for `out_cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iroh_http_try_next_chunk(
    endpoint_handle: u64,
    handle: u64,
    out_ptr: *mut u8,
    out_cap: usize,
) -> i32 {
    if out_cap > 0 && out_ptr.is_null() {
        return -1;
    }

    let ep = match registry::get_endpoint(endpoint_handle) {
        Some(ep) => ep,
        None => return -1,
    };

    match ep.handles().try_next_chunk(handle) {
        Ok(None) => 0, // EOF
        Ok(Some(b)) => {
            let len = b.len();
            if len > out_cap {
                return i32::try_from(len)
                    .map(|n| 0i32.wrapping_sub(n))
                    .unwrap_or(i32::MIN);
            }
            unsafe {
                std::ptr::copy_nonoverlapping(b.as_ptr(), out_ptr, len);
            }
            len as i32
        }
        Err(e) => {
            // Distinguish "channel empty / lock contended" from hard errors.
            if e.message.starts_with("try_next_chunk:") {
                -2 // Transient — no data yet.
            } else {
                record_stream_error(endpoint_handle, handle, e);
                -1 // Hard error (invalid handle, etc.)
            }
        }
    }
}

/// Retrieve and clear structured JSON for the last hard stream error on a handle.
///
/// Return value:
/// - `n > 0`  — bytes written into the buffer.
/// - `n == 0` — no cached error exists.
/// - `n < 0`  — `|n|` bytes required; caller must retry with a larger buffer.
///
/// # Safety
/// `out_ptr` must be valid for `out_cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iroh_http_take_last_stream_error(
    endpoint_handle: u64,
    handle: u64,
    out_ptr: *mut u8,
    out_cap: usize,
) -> i32 {
    if out_cap > 0 && out_ptr.is_null() {
        return -1;
    }

    let key = (endpoint_handle, handle);
    let mut errors = stream_errors().lock().unwrap_or_else(|e| e.into_inner());
    let Some(bytes) = errors.get(&key) else {
        return 0;
    };
    let len = bytes.len();
    if len > out_cap {
        return i32::try_from(len)
            .map(|n| 0i32.wrapping_sub(n))
            .unwrap_or(i32::MIN);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr, len);
    }
    errors.remove(&key);
    len as i32
}

/// Raw-buffer `sendChunk` — bypasses JSON dispatch for streaming throughput.
///
/// Copies `len` bytes from `ptr` into a new chunk and sends it to the body
/// writer at `handle`.
///
/// Return value:
/// - `0`  — success.
/// - `-1` — error (endpoint gone, handle invalid, or channel closed).
///
/// This symbol is declared `nonblocking: true` in the Deno `dlopen` call.
///
/// # Safety
/// `ptr` must be valid for `len` bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iroh_http_send_chunk(
    endpoint_handle: u64,
    handle: u64,
    ptr: *const u8,
    len: usize,
) -> i32 {
    if len > 0 && ptr.is_null() {
        return -1;
    }

    let ep = match registry::get_endpoint(endpoint_handle) {
        Some(ep) => ep,
        None => return -1,
    };

    // SAFETY: Guard above ensures ptr is non-null when len > 0. Use an empty
    // slice for zero-length inputs to avoid null-pointer UB (SEC-001).
    let slice: &[u8] = if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len) }
    };
    let bytes = bytes::Bytes::copy_from_slice(slice);

    match runtime() {
        None => -1,
        Some(rt) => match rt.block_on(ep.handles().send_chunk(handle, bytes)) {
            Ok(()) => 0,
            Err(_) => -1,
        },
    }
}

// ── Synchronous FFI symbols ───────────────────────────────────────────────────
//
// #122: operations that are O(1) slab lookups must NOT go through the async
// `iroh_http_call` path.  Deno's `nonblocking: true` runs each call through
// `spawn_blocking` → `JoinHandle` → tokio poll.  Under concurrent load the
// futures compete for the single-threaded tokio runtime, and long-blocking ops
// (like `nextRequest` awaiting mpsc recv) starve short ops (like `respond`).
//
// By declaring these symbols `nonblocking: false` in the Deno `dlopen` call,
// they execute inline on the JS thread — exactly like Node's `#[napi]` calls.
// This eliminates contention on the request-response critical path.

/// Synchronous respond — send the response head for a pending request.
///
/// `headers_ptr`/`headers_len` encode a JSON array of `[name, value]` pairs.
///
/// Returns `0` on success, `-1` on error.
///
/// # Safety
/// `headers_ptr` must be valid for `headers_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iroh_http_respond(
    endpoint_handle: u64,
    req_handle: u64,
    status: u32,
    headers_ptr: *const u8,
    headers_len: usize,
) -> i32 {
    let ep = match registry::get_endpoint(endpoint_handle) {
        Some(ep) => ep,
        None => return -1,
    };

    let header_pairs: Vec<(String, String)> = if headers_len == 0 {
        Vec::new()
    } else {
        if headers_ptr.is_null() {
            return -1;
        }
        let slice = unsafe { std::slice::from_raw_parts(headers_ptr, headers_len) };
        match serde_json::from_slice::<Vec<Vec<String>>>(slice) {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|p| {
                    if p.len() == 2 {
                        Some((p[0].clone(), p[1].clone()))
                    } else {
                        None
                    }
                })
                .collect(),
            Err(_) => return -1,
        }
    };

    match iroh_http_core::respond(ep.handles(), req_handle, status as u16, header_pairs) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Synchronous finish_body — drop the writer, signalling body-complete.
///
/// Returns `0` on success, `-1` on error.
#[unsafe(no_mangle)]
pub extern "C" fn iroh_http_finish_body(endpoint_handle: u64, handle: u64) -> i32 {
    let ep = match registry::get_endpoint(endpoint_handle) {
        Some(ep) => ep,
        None => return -1,
    };
    match ep.handles().finish_body(handle) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Synchronous cancel_reader — drop a body reader so pending reads return None.
///
/// Returns `0` on success.
#[unsafe(no_mangle)]
pub extern "C" fn iroh_http_cancel_reader(endpoint_handle: u64, handle: u64) -> i32 {
    let ep = match registry::get_endpoint(endpoint_handle) {
        Some(ep) => ep,
        None => return -1,
    };
    ep.handles().cancel_reader(handle);
    0
}

// ── Non-blocking serve request polling ────────────────────────────────────────
//
// #122: The dispatch-based `nextRequest` (via `iroh_http_call` nonblocking:true)
// deadlocks under concurrent load because it competes for Deno's
// `spawn_blocking` thread pool with `rawFetch`.
//
// Fix: `iroh_http_try_next_request` is a sync FFI symbol (`nonblocking: false`)
// that calls `try_recv()` on the serve queue — O(1), never blocks.
// JS polls it in a tight loop with `setTimeout(0)` yield when empty.
// This keeps request delivery on the JS thread, completely bypassing the
// thread pool that `rawFetch` uses.

/// Try to receive the next queued request without blocking.
///
/// Writes a JSON-encoded request payload to `out_ptr[0..out_cap]`.
///
/// Return value:
/// - `n > 0` — bytes written; a request is available.
/// - `0`     — queue empty; call again after yielding.
/// - `-1`    — serve stopped or queue removed; exit the loop.
/// - `n < -1`— buffer too small; `|n|` bytes required.
///
/// # Safety
/// `out_ptr` must be valid for `out_cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iroh_http_try_next_request(
    endpoint_handle: u64,
    out_ptr: *mut u8,
    out_cap: usize,
) -> i32 {
    if out_cap > 0 && out_ptr.is_null() {
        return -1;
    }

    let queue = match serve_registry::get(endpoint_handle) {
        Some(q) => q,
        None => return -1, // serve not started or already stopped
    };

    // Check shutdown first.
    if *queue.shutdown_rx.borrow() {
        return -1;
    }

    // Non-blocking receive from the mpsc queue.
    // We need the mutex to access the receiver.  Use std blocking lock since
    // this runs on the JS thread (not inside a tokio context).
    let mut rx_guard = match queue.rx.try_lock() {
        Ok(g) => g,
        Err(_) => return 0, // another call has the lock; treat as empty
    };

    match rx_guard.try_recv() {
        Ok(event) => {
            drop(rx_guard);
            let encoded = match serde_json::to_vec(&event) {
                Ok(v) => v,
                Err(_) => return -1,
            };
            let len = encoded.len();
            if len > out_cap {
                return i32::try_from(len)
                    .map(|n| 0i32.wrapping_sub(n))
                    .unwrap_or(i32::MIN);
            }
            unsafe {
                std::ptr::copy_nonoverlapping(encoded.as_ptr(), out_ptr, len);
            }
            len as i32
        }
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => 0,
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => -1,
    }
}

/// Force-close all open endpoints.  Call from a Deno signal listener or
/// `unload` event handler so QUIC connections send a proper CONNECTION_CLOSE
/// frame before the process exits.
#[unsafe(no_mangle)]
pub extern "C" fn iroh_http_close_all() {
    // Wake all serve polling loops first so JS handlers can settle before the
    // endpoints they hold are torn out from under them (regression: issue #155).
    serve_registry::shutdown_all();
    registry::close_all_endpoints();
}

// ── Split-fetch: start + poll (#126) ──────────────────────────────────────────
//
// The JSON dispatch path (`iroh_http_call` with `nonblocking: true`) adds
// ~100–200µs of `spawn_blocking` scheduling overhead PER call.  For `rawFetch`
// this is the dominant cost on warm connections.
//
// The split-fetch pattern eliminates this:
//   1. `iroh_http_start_fetch` (sync, `nonblocking: false`) — parses the
//      request JSON, spawns the async fetch as a tokio task, returns a token.
//      Runs on the JS thread; no spawn_blocking overhead.
//   2. `iroh_http_poll_fetch` (sync, `nonblocking: false`) — checks if the
//      tokio task completed.  If done, writes the JSON result to the output
//      buffer.  If still pending, returns 0.
//
// The JS side calls `startFetch`, then polls with `setTimeout(0)` yield
// between iterations.  On warm connections the QUIC round-trip is ~100–200µs,
// so the poll loop typically completes in 1–2 iterations.

/// Start an async fetch on the tokio runtime.
///
/// `request_ptr`/`request_len` — JSON-encoded `RawFetchPayload` (same as the
/// `rawFetch` payload passed to `iroh_http_call`).
///
/// Returns a non-zero token on success, or 0 on error.
///
/// # Safety
/// `request_ptr` must be valid for `request_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iroh_http_start_fetch(request_ptr: *const u8, request_len: usize) -> u64 {
    if request_len > 0 && request_ptr.is_null() {
        return 0;
    }

    let payload: &[u8] = if request_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(request_ptr, request_len) }
    };

    let rt = match runtime() {
        Some(rt) => rt,
        None => return 0,
    };

    // #286: Own the payload bytes and hand them straight to the typed
    // rawFetch fast path — avoids re-parsing to a serde_json::Value here and
    // again in `dispatch`, plus the intermediate `to_vec` re-encode. The
    // borrowed FFI slice is only valid for this synchronous call, so copy it
    // before moving into the worker-thread task.
    let payload_vec = payload.to_vec();

    let token = FETCH_COUNTER.fetch_add(1, Ordering::Relaxed);

    // Spawn the fetch as a tokio task — runs on worker threads, no
    // spawn_blocking overhead.
    let handle = rt.spawn(async move {
        let response = dispatch::raw_fetch_from_slice(&payload_vec).await;
        let encoded = serde_json::to_vec(&response).unwrap_or_else(|e| {
            serde_json::to_vec(&serde_json::json!({ "err": e.to_string() }))
                .expect("static error JSON is always valid")
        });
        Ok(encoded) as FetchResult
    });

    fetch_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(token, handle);

    token
}

/// Start a fetch with callback notification.
///
/// Like `iroh_http_start_fetch`, but instead of requiring JS to poll, the
/// provided callback is invoked (from the tokio worker thread) when the fetch
/// completes.  Deno's `UnsafeCallback` posts the call to the JS event loop,
/// giving µs-precision notification without spin-polling or setTimeout.
///
/// The callback signature is `fn(token: u64)`.  After receiving the callback,
/// call `iroh_http_poll_fetch` once to retrieve the result.
///
/// # Safety
/// - `request_ptr` must be valid for `request_len` bytes.
/// - `callback` must be a valid function pointer that remains valid until called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iroh_http_start_fetch_cb(
    request_ptr: *const u8,
    request_len: usize,
    callback: extern "C" fn(u64),
) -> u64 {
    if request_len > 0 && request_ptr.is_null() {
        return 0;
    }

    let payload: &[u8] = if request_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(request_ptr, request_len) }
    };

    let rt = match runtime() {
        Some(rt) => rt,
        None => return 0,
    };

    let p: serde_json::Value = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(_) => return 0,
    };

    let token = FETCH_COUNTER.fetch_add(1, Ordering::Relaxed);

    let cb = callback;
    rt.spawn(async move {
        let response =
            dispatch::dispatch("rawFetch", &serde_json::to_vec(&p).unwrap_or_default()).await;
        let encoded = serde_json::to_vec(&response).unwrap_or_else(|e| {
            serde_json::to_vec(&serde_json::json!({ "err": e.to_string() }))
                .expect("static error JSON is always valid")
        });
        // Store result before signalling — JS will call poll_fetch to read it.
        fetch_results()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(token, Ok(encoded));
        // Signal JS event loop.
        cb(token);
    });

    token
}

/// Poll for the result of a `start_fetch` call.
///
/// If the fetch is complete, writes the JSON result to `out_ptr[0..out_cap]`.
///
/// Return value:
/// - `n > 0`  — fetch complete; `n` bytes written to output buffer.
/// - `0`      — still pending; call again after yielding.
/// - `-1`     — error (unknown token, task panicked).
/// - `n < -1` — buffer too small; `|n|` bytes required.
///
/// # Safety
/// `out_ptr` must be valid for `out_cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iroh_http_poll_fetch(token: u64, out_ptr: *mut u8, out_cap: usize) -> i32 {
    if out_cap > 0 && out_ptr.is_null() {
        return -1;
    }

    // First check if we already have a result cached from a previous poll.
    {
        let mut results = fetch_results().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(result) = results.remove(&token) {
            match result {
                Ok(encoded) | Err(encoded) => {
                    let len = encoded.len();
                    if len > out_cap {
                        // Put it back — caller needs a bigger buffer.
                        results.insert(token, Ok(encoded));
                        return i32::try_from(len)
                            .map(|n| 0i32.wrapping_sub(n))
                            .unwrap_or(i32::MIN);
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(encoded.as_ptr(), out_ptr, len);
                    }
                    return len as i32;
                }
            }
        }
    }

    // Check if the task has finished.
    let mut registry = fetch_registry().lock().unwrap_or_else(|e| e.into_inner());
    let handle = match registry.get(&token) {
        Some(h) => h,
        None => return -1, // Unknown token.
    };

    if !handle.is_finished() {
        return 0; // Still pending.
    }

    // Task finished — remove from registry and get the result.
    let handle = match registry.remove(&token) {
        Some(h) => h,
        None => return -1,
    };
    drop(registry); // Release lock before blocking on result.

    let rt = match runtime() {
        Some(rt) => rt,
        None => return -1,
    };

    // The task is finished, so block_on is instant.
    let result = match rt.block_on(handle) {
        Ok(r) => r,
        Err(_) => return -1, // Task panicked.
    };

    match result {
        Ok(encoded) | Err(encoded) => {
            let len = encoded.len();
            if len > out_cap {
                // Cache for retry with bigger buffer.
                fetch_results()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(token, Ok(encoded));
                return i32::try_from(len)
                    .map(|n| 0i32.wrapping_sub(n))
                    .unwrap_or(i32::MIN);
            }
            unsafe {
                std::ptr::copy_nonoverlapping(encoded.as_ptr(), out_ptr, len);
            }
            len as i32
        }
    }
}
