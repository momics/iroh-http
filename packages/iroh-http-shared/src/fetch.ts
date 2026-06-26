/**
 * `makeFetch` — wraps the raw platform fetch in the web-standard signature.
 *
 * ```ts
 * const nodeFetch = makeFetch(adapter, endpointHandle);
 * const res = await nodeFetch(peer.toURL('/api/data'));
 * ```
 */

import type { FfiResponse, IrohAdapter, IrohFetchInit } from "./IrohAdapter.js";
import { bodyInitToStream, makeReadable, pipeToWriter } from "./streams.js";
import { classifyError } from "./errors.js";
import { decodeBase64 } from "./utils.js";

export type FetchFn = {
  /** Web-standard form: peer identity is embedded in the `httpi://` URL hostname. */
  (input: string | URL, init?: IrohFetchInit): Promise<Response>;
};

/**
 * Construct a `fetch`-like function bound to a specific `IrohEndpoint`.
 *
 * Supports `AbortSignal` via `init.signal` (§3).
 *
 * @param adapter         Platform adapter implementation (nextChunk, sendChunk, etc.).
 * @param endpointHandle  Slab handle returned by the low-level bind.
 * @returns A `fetch`-like function: `(url, init?) => Promise<Response>`.
 *
 * @example
 * ```ts
 * const doFetch = makeFetch(adapter, handle);
 * const res = await doFetch(peer.toURL('/api/data'), { method: 'POST', body: 'hi' });
 * console.log(await res.text());
 * ```
 */
export function makeFetch(
  adapter: IrohAdapter,
  endpointHandle: number,
): FetchFn {
  return async function irohFetch(
    input: string | URL,
    init?: IrohFetchInit,
  ): Promise<Response> {
    let nodeId: string;
    let url: string;

    const raw = input instanceof URL ? input.href : String(input);
    if (!/^httpi:\/\//i.test(raw)) {
      throw new TypeError(
        `iroh-http fetch() requires an httpi:// URL. ` +
          `Got: "${raw.slice(0, 80)}". ` +
          `Use peer.toURL("/path") to build the URL, e.g. node.fetch(peer.toURL("/ping")).`,
      );
    }
    const parsed = new URL(raw);
    nodeId = parsed.hostname;
    // Strip the URL fragment before it crosses the FFI boundary. Fragments are
    // client-local (RFC 3986 §3.5) and standard HTTP clients never transmit
    // them; forwarding one would leak local-only data (tokens, UI state,
    // deep-link secrets) to the remote peer. Drop everything from the first
    // '#', which always delimits the fragment.
    url = raw.replace(/#.*$/s, "");

    const method = init?.method ?? "GET";
    const signal = init?.signal ?? null;
    const directAddrs = init?.directAddrs ?? null;
    const fetchOptions = {
      timeoutMs: init?.requestTimeout,
      decompress: init?.decompress,
      maxResponseBodyBytes: init?.maxResponseBodyBytes,
    };

    // Reject GET and HEAD request bodies — matches web-platform fetch semantics
    // (https://fetch.spec.whatwg.org/#concept-method-normalize, issue-58).
    if (
      (method.toUpperCase() === "GET" || method.toUpperCase() === "HEAD") &&
      init?.body != null
    ) {
      throw new TypeError(
        `Request body is not allowed for ${method} requests. ` +
          `The web-platform fetch specification forbids bodies on GET and HEAD.`,
      );
    }

    // Reject immediately if already aborted.
    if (signal?.aborted) {
      throw new DOMException("The operation was aborted", "AbortError");
    }

    const headers: [string, string][] = normaliseHeaders(init?.headers);

    // Allocate request body writer if needed.  The pipe is started AFTER
    // rawFetch is dispatched — see below.
    let reqBodyHandle: bigint | null = null;
    let bodyPipePromise: Promise<void> | null = null;
    const bodyStream = init?.body ? bodyInitToStream(init.body) : null;
    if (bodyStream) {
      reqBodyHandle = await adapter.allocBodyWriter(endpointHandle);
    }

    // #126: Allocate a fetch cancellation token when an AbortSignal is
    // provided.  Without a signal the token is unused overhead — one FFI
    // round-trip saved on the hot path.  When skipped, rawFetch passes 0n
    // and the Rust dispatch allocates an internal token automatically.
    //
    // A token is also allocated whenever there is a request body so that a
    // mid-stream body failure can RESET the request stream (see the body-pipe
    // wiring below). Without a token the only way to release the body is a
    // clean finish, which would truncate the body into a "successful" close.
    let fetchToken: bigint = 0n;
    if (signal || bodyStream) {
      fetchToken = await adapter.allocFetchToken(endpointHandle);
    }

    // Wire AbortSignal → cancelFetch as early as possible (fire-and-forget).
    // This fires even if the signal is already aborted.
    let cancelAbortListener: (() => void) | null = null;
    if (signal) {
      if (signal.aborted) {
        adapter.cancelFetch(fetchToken);
        throw new DOMException("The operation was aborted", "AbortError");
      }
      cancelAbortListener = () => adapter.cancelFetch(fetchToken);
      signal.addEventListener("abort", cancelAbortListener, { once: true });
    }

    // Build an abort promise for the JS-side race (still needed so the Promise
    // rejects immediately while the Rust cancel propagates in the background).
    let onAbort: (() => void) | null = null;
    const abortPromise = signal
      ? new Promise<never>((_, reject) => {
        onAbort = () =>
          reject(new DOMException("The operation was aborted", "AbortError"));
        signal.addEventListener("abort", onAbort);
      })
      : null;

    // Dispatch rawFetch to the Rust runtime FIRST (non-blocking — it runs on
    // spawn_blocking / tokio worker threads).  This ensures the Rust side
    // claims the body reader and begins the QUIC handshake before JS starts
    // pushing body chunks.  Without this ordering, a sync sendChunk call on a
    // large body blocks the JS thread before rawFetch is dispatched, starving
    // the body reader and causing a drain timeout (#122 follow-up).
    const fetchPromise = adapter.rawFetch(
      endpointHandle,
      nodeId,
      url,
      method,
      headers,
      reqBodyHandle,
      fetchToken,
      directAddrs,
      fetchOptions,
    );

    // NOW start the body pipe — rawFetch has been dispatched to Rust, so the
    // body reader will have a consumer by the time the channel fills up.
    if (bodyStream && reqBodyHandle !== null) {
      bodyPipePromise = pipeToWriter(adapter, bodyStream, reqBodyHandle);
      // If the body source fails mid-stream, reset the request stream instead
      // of letting it finish cleanly. A clean finish would present a truncated
      // body to the peer as complete; resetting via cancelFetch makes the peer
      // observe an aborted request. Attached at creation (not after the head)
      // so the reset fires even when the failure precedes the response head.
      bodyPipePromise.catch((err) => {
        console.error(
          "[iroh-http] request body pipe failed; aborting request:",
          err,
        );
        if (fetchToken !== 0n) adapter.cancelFetch(fetchToken);
      });
    }

    let rawRes: FfiResponse;
    try {
      rawRes = abortPromise
        ? await Promise.race([fetchPromise, abortPromise])
        : await fetchPromise;
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") throw err;
      throw classifyError(err);
    } finally {
      if (signal && onAbort) signal.removeEventListener("abort", onAbort);
      // Remove the transport-cancel listener once the response head is received.
      if (signal && cancelAbortListener) {
        signal.removeEventListener("abort", cancelAbortListener);
        cancelAbortListener = null;
      }
    }

    // Core returns body_handle = 0 (the slotmap null sentinel) for null-body
    // status codes (RFC 9110 §6.3: 204, 205, 304).  No channel was allocated
    // on the Rust side, so there is nothing to wire up here.
    let responseBody: ReadableStream<Uint8Array> | null = null;

    if (rawRes.inlineBody != null) {
      // #126: Body was eagerly read on the Rust side and returned inline
      // (base64-encoded) to avoid extra nextChunk FFI round-trips.
      const decoded = decodeBase64(rawRes.inlineBody);
      responseBody = new ReadableStream<Uint8Array>({
        start(controller) {
          if (decoded.byteLength > 0) {
            controller.enqueue(decoded);
          }
          controller.close();
        },
      });
    } else if (rawRes.bodyHandle !== 0n) {
      // Wire AbortSignal to cancel the body reader (§3).
      // Uses `once: true` so the listener auto-removes after firing — prevents
      // leaks when the response body is never consumed by the caller.
      let cancelOnAbort: (() => void) | null = null;
      if (signal) {
        cancelOnAbort = () => adapter.cancelRequest(rawRes.bodyHandle);
        signal.addEventListener("abort", cancelOnAbort, { once: true });
      }

      // Wrap response body in a ReadableStream.
      // When the stream closes (EOF or cancel), remove the abort listener to avoid a leak.
      responseBody = makeReadable(adapter, rawRes.bodyHandle, () => {
        if (signal && cancelOnAbort) {
          signal.removeEventListener("abort", cancelOnAbort!);
          cancelOnAbort = null;
        }
      });
    }

    const response = new Response(responseBody, {
      status: rawRes.status,
      headers: rawRes.headers,
    });

    // Shadow the read-only Response.url with the httpi:// address (§brief).
    Object.defineProperty(response, "url", {
      value: rawRes.url,
      writable: false,
      enumerable: false,
      configurable: true,
    });

    return response;
  };
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Shared empty tuple — avoids allocating `[]` on every no-header request. */
const EMPTY_HEADERS: [string, string][] = [];

function normaliseHeaders(
  h: HeadersInit | undefined | null,
): [string, string][] {
  if (!h) return EMPTY_HEADERS;
  // Array is the most common shape from adapters — check first.
  if (Array.isArray(h)) return h as [string, string][];
  if (h instanceof Headers) {
    const pairs: [string, string][] = [];
    h.forEach((v, k) => pairs.push([k, v]));
    return pairs;
  }
  return Object.entries(h) as [string, string][];
}
