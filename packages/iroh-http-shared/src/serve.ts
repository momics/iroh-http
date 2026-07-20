/**
 * `makeServe` — wraps the raw platform serve in a Deno-compatible signature.
 *
 * ```ts
 * const serve = makeServe(adapter, handle, onNodeClose);
 * const server = serve(async (req) => Response.json({ ok: true }));
 * await server.finished;
 * ```
 */

import type {
  FfiResponseHead,
  FfiServeOptions,
  IrohAdapter,
  PeerConnectionEvent,
  RequestPayload,
} from "./IrohAdapter.js";
import { makeReadable, pipeToWriter } from "./streams.js";
import { classifyError } from "./errors.js";

/**
 * A request handler that receives a web-standard `Request` and returns a `Response`.
 *
 * The `Request` is augmented with:
 * - `req.headers.get('Peer-Id')` — the authenticated peer's public key.
 *
 * ## Security
 *
 * `serve()` opens a **public endpoint** on the Iroh overlay network. Unlike
 * regular HTTP (where binding on localhost keeps you private), any peer that
 * knows or discovers your node's public key can connect and send requests.
 * Iroh QUIC authenticates the peer's *identity*, but not *authorization*.
 *
 * Always check `Peer-Id` and reject requests from untrusted peers:
 *
 * ```ts
 * const ALLOWED_PEERS = new Set(["<peer-public-key>"]);
 * node.serve({}, (req) => {
 *   const peerId = req.headers.get("Peer-Id");
 *   if (!ALLOWED_PEERS.has(peerId)) return new Response("Forbidden", { status: 403 });
 *   return new Response("ok");
 * });
 * ```
 */
export type ServeHandler = (req: Request) => Response | Promise<Response>;

/**
 * Options for the `serve()` call.
 *
 * All fields are optional.  The handler can be passed here (single-argument
 * form) or as a separate second argument.
 */
export interface ServeOptions {
  /**
   * Called when a request handler throws or rejects.
   *
   * The returned `Response` is sent to the client.  If this callback also
   * throws, the request receives a bare `500 Internal Server Error`.
   *
   * @default Returns `500 Internal Server Error` with no body.
   */
  onError?: (error: unknown) => Response | Promise<Response>;

  /**
   * When the signal is aborted, the serve loop stops accepting new
   * connections and drains in-flight requests (graceful shutdown).
   *
   * This only stops the serve loop — the node itself stays alive.
   */
  signal?: AbortSignal;

  /**
   * Maximum simultaneous in-flight requests.
   * @default 1024
   */
  maxConcurrency?: number;

  /**
   * Maximum connections from a single peer.
   * @default 8
   */
  maxConnectionsPerPeer?: number;

  /**
   * Per-request timeout in milliseconds.  0 = disabled.
   * @default 60_000
   */
  requestTimeout?: number;

  /**
   * Reject request bodies larger than this many **wire** (compressed) bytes.
   * Guards against high-bandwidth floods at the network level.
   * @default 16_777_216 (16 MiB)
   */
  maxRequestBodyWireBytes?: number;

  /**
   * Reject request bodies larger than this many **decoded** bytes (after
   * decompression). This is the primary compression-bomb guard — a zstd
   * payload that is tiny on the wire but expands to GB is caught here.
   * @default 16_777_216 (16 MiB)
   */
  maxRequestBodyDecodedBytes?: number;

  /**
   * Maximum total QUIC connections the server will accept.
   * @default unlimited
   */
  maxTotalConnections?: number;

  /**
   * Milliseconds to wait for in-flight requests to drain after shutdown.
   * @default 30_000
   */
  drainTimeout?: number;

  /**
   * When `true`, reject requests with 503 when at capacity instead of queuing.
   * @default true
   */
  loadShed?: boolean;

  /**
   * When `false`, request bodies are forwarded raw to the handler without
   * decompression.  Useful for relay/proxy nodes that forward bodies
   * downstream without inspecting them.
   * @default true
   */
  decompress?: boolean;
}

/**
 * Handle returned by `serve()`.
 */
export interface ServeHandle {
  /**
   * Resolves when the serve loop terminates — either because `close()` or
   * `node.close()` was called, `signal` was aborted, or a fatal error occurred.
   */
  readonly finished: Promise<void>;

  /**
   * Stop **this** server: signals the serve loop to stop accepting new
   * connections and drain in-flight requests (graceful shutdown), leaving the
   * node itself alive so other work can continue.
   *
   * Resolves once the loop has fully terminated — it is equivalent to calling
   * the stop and then awaiting {@link ServeHandle.finished}. Safe to call more
   * than once; subsequent calls simply await the same shutdown.
   */
  close(): Promise<void>;

  /**
   * Alias for {@link ServeHandle.close} enabling `await using server = ...`
   * (explicit resource management). Stops this server when the enclosing scope
   * exits.
   */
  [Symbol.asyncDispose](): Promise<void>;
}

/**
 * Two overloaded call signatures for `serve()`:
 *
 * 1. `serve(handler)` — handler only (most common).
 * 2. `serve(options, handler)` — options + separate handler argument.
 */
export type ServeFn = {
  (handler: ServeHandler): ServeHandle;
  (options: ServeOptions, handler: ServeHandler): ServeHandle;
};

/**
 * HTTP methods that categorically cannot carry a request body per RFC 9110.
 * All other methods — including OPTIONS, custom verbs, etc. — may carry a body
 * and should have the body stream forwarded to the handler (issue-58 fix).
 */
const METHODS_WITHOUT_BODY = new Set(["GET", "HEAD", "CONNECT", "TRACE"]);

/**
 * Construct a Deno-compatible `serve` function bound to a specific endpoint.
 *
 * @param adapter         Platform adapter implementation (sendChunk, finishBody, etc.).
 * @param endpointHandle  Slab handle returned by the low-level bind.
 * @param onNodeClose     Promise that resolves when the node shuts down.
 * @param onPeerEvent     Optional callback for peer connect/disconnect events.
 * @returns A `serve` function with three overloaded call signatures.
 *
 * @example
 * ```ts
 * const server = serve(async (req) => {
 *   const peer = req.headers.get('Peer-Id');
 *   return Response.json({ echo: await req.text(), peer });
 * });
 * await server.finished;
 * ```
 */
export function makeServe(
  adapter: IrohAdapter,
  endpointHandle: number,
  onNodeClose: Promise<void>,
  onPeerEvent?: (event: PeerConnectionEvent) => void,
  maxChunkSizeBytes?: number,
): ServeFn {
  // #114: guard against starting two polling loops on the same endpoint.
  let serveRunning = false;

  return ((...args: unknown[]): ServeHandle => {
    if (serveRunning) {
      throw new TypeError(
        "serve() is already running on this node. Call signal.abort() or node.close() to stop it first.",
      );
    }
    serveRunning = true;

    // #277: ensure the underlying stopServe() is only issued once even if
    // close(), Symbol.asyncDispose, and/or an aborted signal all fire.
    let stopRequested = false;

    // Parse overloaded arguments.
    let handler: ServeHandler;
    let options: ServeOptions = {};

    if (typeof args[0] === "function") {
      // serve(handler)
      handler = args[0] as ServeHandler;
    } else if (args.length >= 2 && typeof args[1] === "function") {
      // serve(options, handler)
      options = (args[0] as ServeOptions) ?? {};
      handler = args[1] as ServeHandler;
    } else {
      throw new TypeError("serve() requires a handler function");
    }

    const onError = options.onError ?? defaultOnError;

    // Peer connect/disconnect events are dispatched as CustomEvents on IrohNode.
    // The dispatcher is provided by IrohNode at construction time so events fire
    // on the node regardless of which serve() call is running.
    const onConnectionEvent:
      | ((event: PeerConnectionEvent) => void)
      | undefined = onPeerEvent;

    // #119: Track active body pipes so `finished` drains them before resolving.
    const activePipes = new Set<Promise<void>>();

    // Build FFI-level serve options from the user-facing ServeOptions.
    const ffiServeOpts: FfiServeOptions = {
      maxConcurrency: options.maxConcurrency,
      maxConnectionsPerPeer: options.maxConnectionsPerPeer,
      requestTimeout: options.requestTimeout,
      maxRequestBodyWireBytes: options.maxRequestBodyWireBytes,
      maxRequestBodyDecodedBytes: options.maxRequestBodyDecodedBytes,
      maxTotalConnections: options.maxTotalConnections,
      drainTimeout: options.drainTimeout,
      loadShed: options.loadShed,
      decompress: options.decompress,
    };

    // rawServe returns a Promise<void> that resolves when its internal polling
    // loop exits (i.e. after stopServe() causes nextRequest to drain to null).
    const loopDone = adapter.rawServe(
      endpointHandle,
      { onConnectionEvent, serveOptions: ffiServeOpts },
      async (payload: RequestPayload): Promise<FfiResponseHead> => {
        const peerId = headerValue(payload.headers, "peer-id");

        // Build a web-standard Request.
        const hasBody = !METHODS_WITHOUT_BODY.has(payload.method.toUpperCase());
        const reqBody = hasBody
          ? makeReadable(adapter, payload.reqBodyHandle)
          : null;

        // Peer-Id is stripped (spoof prevention) and re-injected from the
        // authenticated QUIC connection identity in Rust core. No duplication here.
        const headers: [string, string][] = [...payload.headers];
        const inboundHeaders = new Headers(headers);

        const reqInit: RequestInit & { duplex?: "half" } = {
          method: payload.method,
          headers,
          body: reqBody,
        };
        if (reqBody) reqInit.duplex = "half";

        const req = new Request(
          payload.url,
          reqInit,
        );

        // Chromium-based runtimes may still apply the forbidden request-header
        // filter while constructing a Request, even though this is an inbound
        // server request and the native transport has already received the
        // headers. Expose that authoritative set to the handler rather than the
        // constructor's potentially filtered copy. This is intentionally
        // header-agnostic so platform policy cannot silently discard other
        // legitimate inbound fields.
        Object.defineProperty(req, "headers", {
          value: inboundHeaders,
          configurable: true,
          enumerable: true,
        });

        // Invoke the user handler with onError fallback.
        let res: Response;
        let handlerFailed = false;
        try {
          const returned = await Promise.resolve(handler(req));
          if (!(returned instanceof Response)) {
            throw new TypeError(
              `serve handler must return a Response, got ${typeof returned}`,
            );
          }
          res = returned;
        } catch (err) {
          handlerFailed = true;
          try {
            res = await Promise.resolve(onError(err));
          } catch {
            res = new Response("Internal Server Error", { status: 500 });
          }
        }

        // #338: Some platforms — notably the Android System WebView — return a
        // falsy `res.body` for a body-carrying Response (`new Response("text")`
        // yields no usable ReadableStream). `res.body ?? emptyStream()` then
        // silently dropped the body, so remote peers saw the correct status and
        // headers but zero bytes. When `res.body` is absent, buffer the bytes via
        // `res.arrayBuffer()` into a one-shot stream (empty for a genuine 204/304).
        // When `res.body` is present we use it directly, so streaming responses on
        // desktop/iOS/Node/Deno are never forced to fully buffer (no regression).
        let bodyStream: ReadableStream<Uint8Array>;
        if (res.body) {
          bodyStream = res.body;
        } else {
          const buffered = new Uint8Array(await res.arrayBuffer());
          bodyStream = new ReadableStream<Uint8Array>({
            start(controller) {
              if (buffered.byteLength > 0) controller.enqueue(buffered);
              controller.close();
            },
          });
        }
        const doPipe = async () => {
          await pipeToWriter(adapter, bodyStream, payload.resBodyHandle, {
            maxChunkSizeBytes,
          });
        };

        // #123: When the handler failed, the error-recovery body is short
        // (typically empty or a small string).  Await the pipe before returning
        // the response head so that rawRespond fires only after the body
        // handles are fully consumed.  Without this, rawRespond wakes the core
        // handler task which may complete and clean up handles before
        // pipeToWriter calls finishBody, producing spurious "unknown handle"
        // errors.
        //
        // For the normal (non-error) path the pipe runs concurrently — required
        // for streaming responses where the body may be large or unbounded.
        if (handlerFailed) {
          await doPipe().catch((err) =>
            console.error(
              "[iroh-http] response body pipe error:",
              classifyError(err),
            )
          );
        } else {
          const p = doPipe().catch((err) =>
            console.error(
              "[iroh-http] response body pipe error:",
              classifyError(err),
            )
          );
          activePipes.add(p);
          p.finally(() => activePipes.delete(p));
        }

        return {
          status: res.status,
          headers: [...res.headers] as [string, string][],
        };
      },
    );

    // ISS-029 / #59 / #115: finished resolves when the serve loop actually terminates.
    // `loopDone` is the real loop-lifetime promise returned by rawServe():
    //  - Deno: resolves when the nextRequest polling loop exits (null sentinel).
    //  - Node / Tauri: resolves when waitServeStop() confirms the Rust task drained.
    //
    // #115: finished must NOT resolve via onNodeClose alone. When the node closes,
    // close_endpoint() calls serve_registry::remove() which sends the shutdown signal
    // to the pending nextRequest Tokio task — but that task is scheduled, not yet run.
    // If finished resolved immediately on onNodeClose, the nextRequest FFI op would
    // still be in-flight (Deno sanitizeOps / process exit timing bug).
    //
    // Fix: when onNodeClose fires, chain it on loopDone. loopDone is guaranteed to
    // resolve because close_endpoint always calls serve_registry::remove first, which
    // unblocks the pending nextRequest. If loopDone resolves first (normal path when
    // stopServe was called explicitly), the race wins immediately.
    // #119: drain all in-flight body pipes before resolving so callers of
    // `await finished` see a true "all work done" guarantee.
    const finished: Promise<void> = Promise.race([
      loopDone,
      onNodeClose.then(() => loopDone),
    ])
      .then(() => Promise.allSettled([...activePipes]))
      .then(() => {});
    // Reset guard when the loop finishes so serve() can be called again.
    // Use `.then(onFulfilled, onRejected)` (not `.finally`) so the reset runs
    // exactly one microtask after `finished` settles — matching the original
    // `.finally` timing that rapid serve/stop/serve cycles depend on (#119) —
    // while also handling a rejected `finished` (e.g. serve options rejected by
    // the adapter's numeric caps) so it does not surface as an unhandled
    // rejection through this internal bookkeeping chain. Callers still observe
    // the rejection via the `finished` promise they hold.
    const resetGuard = (): void => {
      serveRunning = false;
    };
    void finished.then(resetGuard, resetGuard);

    const doStop = (): void => {
      if (stopRequested) return;
      stopRequested = true;
      adapter.stopServe(endpointHandle);
      // Rust will drain the loop and then loopDone resolves; we do NOT resolve
      // finished ourselves here.
    };

    // Wire signal → doStop for graceful shutdown.
    if (options.signal) {
      if (options.signal.aborted) {
        doStop();
      } else {
        options.signal.addEventListener("abort", () => doStop(), {
          once: true,
        });
      }
    }

    // #277: close() stops THIS server (not the whole node) by triggering the
    // same stop path as an aborted signal, then resolves once the loop drains.
    const close = (): Promise<void> => {
      doStop();
      return finished;
    };

    return {
      finished,
      close,
      [Symbol.asyncDispose]: close,
    };
  }) as ServeFn;
}

function defaultOnError(error: unknown): Response {
  console.error("[iroh-http] unhandled handler error:", error);
  return new Response("Internal Server Error", { status: 500 });
}

function headerValue(
  headers: [string, string][],
  name: string,
): string | null {
  const needle = name.toLowerCase();
  for (const [k, v] of headers) {
    if (k.toLowerCase() === needle) return v;
  }
  return null;
}
