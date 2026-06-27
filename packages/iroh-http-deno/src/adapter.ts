/**
 * iroh-http-deno — DenoAdapter.
 *
 * Implements IrohAdapter via Deno.dlopen FFI.
 */

import { dirname, fromFileUrl, resolve } from "@std/path";
import type {
  EndpointInfo,
  EndpointStats,
  NodeAddrInfo,
  NodeOptions,
  PathInfo,
  PeerConnectionEvent,
  PeerDiscoveryEvent,
  PeerStats,
} from "@momics/iroh-http-shared";
import { IrohAdapter } from "@momics/iroh-http-shared/adapter";
import type {
  AllocBodyWriterFn,
  FetchOptions,
  FfiDuplexStream,
  FfiResponse,
  FfiResponseHead,
  RawFetchFn,
  RawServeFn,
  RawSessionFns,
  RequestPayload,
  TransportEventPayload,
} from "@momics/iroh-http-shared/adapter";
import {
  classifyBindError,
  classifyError,
  decodeBase64,
  encodeBase64,
  isEndpointGoneError,
  normaliseRelayMode,
} from "@momics/iroh-http-shared";
import { download } from "@denosaurs/plug";
import { NATIVE_CHECKSUMS } from "./checksums.ts";

// ── Platform library resolution ───────────────────────────────────────────────

function libExtension(): string {
  switch (Deno.build.os) {
    case "darwin":
      return "dylib";
    case "windows":
      return "dll";
    default:
      return "so";
  }
}

function libName(): string {
  return `libiroh_http_deno.${Deno.build.os}-${Deno.build.arch}.${libExtension()}`;
}

/** Version must match the tag used for GitHub releases (v0.1.0 → tag v0.1.0). */
const VERSION = "0.5.2";
const DOWNLOAD_BASE =
  `https://github.com/momics/iroh-http/releases/download/v${VERSION}`;

/** True if `path` exists and is a regular file. */
async function isFile(path: string): Promise<boolean> {
  try {
    return (await Deno.stat(path)).isFile;
  } catch {
    return false;
  }
}

/**
 * Path to a locally built native library, if running from a source checkout.
 * Returns `undefined` for published (remote) consumers, who always download a
 * release asset instead.
 */
function localLibPath(): string | undefined {
  if (!import.meta.url.startsWith("file://")) return undefined;
  return resolve(dirname(fromFileUrl(import.meta.url)), "..", "lib", libName());
}

/** Lowercase hex SHA-256 of `bytes`. */
async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as BufferSource);
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * Verify the native library at `libPath` against its embedded SHA-256 before it
 * is ever made executable or `dlopen`-ed. Native FFI code runs outside Deno's
 * permission sandbox, so a tampered binary is arbitrary code execution.
 *
 * Policy:
 *   - Expected digest known → must match, else the file is removed and we throw.
 *   - No expected digest:
 *       - freshly `downloaded` → fail closed (never load unverified remote code);
 *       - pre-existing local build → allowed with a warning (source builds have
 *         no stable published digest).
 */
async function verifyLib(
  libPath: string,
  name: string,
  downloaded: boolean,
): Promise<void> {
  const expected = NATIVE_CHECKSUMS[name]?.toLowerCase();
  if (!expected) {
    if (downloaded) {
      try {
        await Deno.remove(libPath);
      } catch {
        /* ignore */
      }
      throw new Error(
        `[iroh-http] Refusing to load downloaded native library "${name}" ` +
          `without an integrity checksum (none embedded for v${VERSION}). ` +
          `This guards against tampered release assets. Build from source instead.`,
      );
    }
    console.error(
      `[iroh-http] WARNING: loading native library "${name}" without integrity ` +
        `verification (no embedded checksum). Expected only for local source builds.`,
    );
    return;
  }

  const actual = await sha256Hex(await Deno.readFile(libPath));
  if (actual !== expected) {
    // Only a downloaded/cached copy is removed on mismatch; never delete a
    // user's locally built library.
    if (downloaded) {
      try {
        await Deno.remove(libPath);
      } catch {
        /* ignore */
      }
    }
    throw new Error(
      `[iroh-http] Native library integrity check FAILED for "${name}".\n` +
        `  expected SHA-256: ${expected}\n` +
        `  actual   SHA-256: ${actual}\n` +
        `  Refusing to load potentially tampered native code.`,
    );
  }
}

async function ensureLib(): Promise<string> {
  const name = libName();

  // 1. Source checkout (dev / CI): use the locally built library directly.
  //    Local builds have no published digest, so verifyLib warns rather than
  //    enforcing when no checksum is embedded.
  const local = localLibPath();
  if (local && (await isFile(local))) {
    await verifyLib(local, name, false);
    return local;
  }

  // 2. Published consumer: let @denosaurs/plug fetch + cache the release asset
  //    (download, cross-platform paths, atomic writes, cache-dir resolution),
  //    then verify its SHA-256 before we hand it to dlopen. plug performs no
  //    integrity check of its own — that gap is exactly what we close here.
  const url = `${DOWNLOAD_BASE}/${name}`;
  let libPath = await download({ url });
  try {
    await verifyLib(libPath, name, true);
  } catch (e) {
    // A cached copy failed verification (e.g. a poisoned cache) — force one
    // clean re-download, then re-verify. A second failure propagates.
    console.error(
      `[iroh-http] cached native library rejected, reloading: ` +
        `${(e as Error).message}`,
    );
    libPath = await download({ url, cache: "reloadAll" });
    await verifyLib(libPath, name, true);
  }
  return libPath;
}

const LIB_PATH = await ensureLib();

// ── FFI symbols ───────────────────────────────────────────────────────────────

const lib = Deno.dlopen(
  LIB_PATH,
  {
    iroh_http_call: {
      parameters: ["buffer", "usize", "buffer", "usize", "buffer", "usize"],
      result: "i32",
      nonblocking: true,
    },
    iroh_http_next_chunk: {
      parameters: ["u64", "u64", "buffer", "usize"],
      result: "i32",
      nonblocking: true,
    },
    // #126: Sync non-blocking variant — returns immediately without
    // entering the spawn_blocking pool.  Returns -2 when no data is
    // available yet (fall back to async nextChunk).
    iroh_http_try_next_chunk: {
      parameters: ["u64", "u64", "buffer", "usize"],
      result: "i32",
      nonblocking: false,
    },
    iroh_http_send_chunk: {
      parameters: ["u64", "u64", "buffer", "usize"],
      result: "i32",
      // Must be async (nonblocking:true) — sync blocks the JS thread, which
      // deadlocks when client and server share the same event loop and the
      // bounded channel fills up (server can't read → QUIC stalls → channel
      // stays full → JS thread stays blocked).  Chunk splitting in pipeToWriter
      // keeps each send O(1) so spawn_blocking overhead is minimal.
      nonblocking: true,
    },
    // #122: Sync symbols — execute inline on JS thread, no spawn_blocking.
    iroh_http_respond: {
      parameters: ["u64", "u64", "u32", "buffer", "usize"],
      result: "i32",
      nonblocking: false,
    },
    iroh_http_finish_body: {
      parameters: ["u64", "u64"],
      result: "i32",
      nonblocking: false,
    },
    iroh_http_cancel_reader: {
      parameters: ["u64", "u64"],
      result: "i32",
      nonblocking: false,
    },
    iroh_http_close_all: {
      parameters: [],
      result: "void",
      nonblocking: false,
    },
    // #122: Sync non-blocking poll for next queued request.
    // Returns immediately — never blocks the JS thread.
    iroh_http_try_next_request: {
      parameters: ["u64", "buffer", "usize"],
      result: "i32",
      nonblocking: false,
    },
    // #126: Split-fetch — sync start + sync poll, bypasses spawn_blocking.
    iroh_http_start_fetch: {
      parameters: ["buffer", "usize"],
      result: "u64",
      nonblocking: false,
    },
    // #126: Callback-based fetch — Rust calls the callback when done.
    iroh_http_start_fetch_cb: {
      parameters: ["buffer", "usize", "pointer"],
      result: "u64",
      nonblocking: false,
    },
    iroh_http_poll_fetch: {
      parameters: ["u64", "buffer", "usize"],
      result: "i32",
      nonblocking: false,
    },
  } as const,
);

// ── Fast event-loop yield ─────────────────────────────────────────────────────
//
// #126: MessageChannel.postMessage gives ~0.017ms yields vs setTimeout(0)'s
// ~2.5ms in Deno.  This is critical for same-process client+server latency
// where the serve loop and fetch completion both need the event loop.
//
// Creates a fresh channel per call to avoid keeping the event loop alive
// when all serve loops have stopped.

function createYieldFn(): {
  yield: () => Promise<void>;
  cancel: () => void;
  close: () => void;
} {
  const ch = new MessageChannel();
  let pendingResolve: (() => void) | null = null;
  return {
    yield: () =>
      new Promise<void>((resolve) => {
        pendingResolve = resolve;
        ch.port2.onmessage = () => {
          pendingResolve = null;
          resolve();
        };
        ch.port1.postMessage(undefined);
      }),
    // #115: Immediately resolve any in-flight yield so the polling loop
    // can re-poll try_next_request and see the -1 shutdown sentinel
    // without waiting for the next MessageChannel tick.
    cancel: () => {
      pendingResolve?.();
      pendingResolve = null;
    },
    close: () => {
      ch.port1.close();
      ch.port2.close();
    },
  };
}

// ── Per-endpoint serve cancellation ───────────────────────────────────────────
//
// #115: stopServe fires an async FFI call. The polling loop may be suspended
// in `await yielder.yield()` when the Rust side processes the stop. This map
// lets stopServe immediately cancel the yield so the loop re-polls and sees
// the -1 shutdown sentinel without a timing-dependent delay.
const serveCancellers = new Map<number, () => void>();

// #245: The `stopServe` FFI call is a non-blocking op (`iroh_http_call`).
// If it is left floating it can still be in flight when a test's `sanitizeOps`
// snapshot is taken, which Deno's leak sanitizer reports as a leaked op. This
// map hands the in-flight promise to the serve loop so it can drain the op
// before resolving `finished`. Entries self-clean once the op settles.
const serveStopOps = new Map<number, Promise<unknown>>();

// ── JSON dispatch helper ──────────────────────────────────────────────────────

const enc = new TextEncoder();
const dec = new TextDecoder();

/**
 * Capacity hint for per-call output buffers.  Each call allocates its own
 * buffer of this size so concurrent `nonblocking` FFI calls never alias the
 * same memory.  Grows when any call needs more space; capped at 4 MB to
 * prevent unbounded memory growth from a single large response.
 */
const MAX_OUT_BUF = 4 * 1024 * 1024; // 4 MB
let outBufHint = 128 * 1024;

/** Pre-encoded method name buffers (UTF-8). */
const METHOD_BUFS: Record<string, Uint8Array> = Object.fromEntries(
  [
    "finishBody",
    "cancelRequest",
    "rawFetch",
    "serveStart",
    "nextRequest",
    "nextConnectionEvent",
    "respond",
    "allocBodyWriter",
    "createEndpoint",
    "closeEndpoint",
    "allocFetchToken",
    "cancelInFlight",
    "waitEndpointClosed",
    "endpointStats",
    "nodeAddr",
    "homeRelay",
    "peerInfo",
    "startTransportEvents",
    "nextTransportEvent",
    "nextPathChange",
    "sessionAccept",
  ].map((m) => [m, enc.encode(m)]),
);

// ISS-032 / #252: bigint handles are slotmap u64 keys (32-bit slot + 32-bit
// version); practical values are well within Number.MAX_SAFE_INTEGER. Convert
// to a JS number only after asserting the safe range, throwing early rather
// than silently corrupting resource identity. Shared by the generic `call()`
// path and the `rawFetch` fast-path so both fail loudly and identically.
export function bigintToSafeNumber(value: bigint, field = "handle"): number {
  if (value < 0n || value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new RangeError(
      `[iroh-http] ${field} value ${value} exceeds safe integer range and cannot be safely serialised`,
    );
  }
  return Number(value);
}

/** Reusable buffer hint for estimating output size of `call()` responses. */ async function call<
  T,
>(method: string, payload: unknown): Promise<T> {
  const methodBuf = METHOD_BUFS[method] ?? enc.encode(method);
  const payloadBuf = enc.encode(
    JSON.stringify(
      payload,
      (_k, v) => typeof v === "bigint" ? bigintToSafeNumber(v) : v,
    ),
  );
  // Deno's FFI types require Uint8Array<ArrayBuffer>; TextEncoder returns
  // Uint8Array<ArrayBufferLike>. The backing store is always a plain ArrayBuffer
  // in practice — cast to satisfy the stricter type.
  const mb = methodBuf as Uint8Array<ArrayBuffer>;
  const pb = payloadBuf as Uint8Array<ArrayBuffer>;

  // Per-call buffer: concurrent nonblocking FFI calls must not share memory.
  // Use the global hint as the initial capacity so most calls allocate once.
  let buf = new Uint8Array(outBufHint) as Uint8Array<ArrayBuffer>;

  let n = (await lib.symbols.iroh_http_call(
    mb,
    BigInt(mb.byteLength),
    pb,
    BigInt(pb.byteLength),
    buf,
    BigInt(buf.byteLength),
  )) as number;

  if (n < 0) {
    // Output buffer too small.  The Rust side cached the response and wrote
    // an 8-byte retrieval token into the first bytes of `buf`.  Read it and
    // retry with method "__cached" to avoid re-dispatching the original
    // handler (DENO-007).
    const tokenBuf = new Uint8Array(
      buf.buffer,
      buf.byteOffset,
      8,
    ) as Uint8Array<ArrayBuffer>;
    const cachedMethod = enc.encode("__cached") as Uint8Array<ArrayBuffer>;
    buf = new Uint8Array(-n) as Uint8Array<ArrayBuffer>;
    n = (await lib.symbols.iroh_http_call(
      cachedMethod,
      BigInt(cachedMethod.byteLength),
      tokenBuf,
      BigInt(tokenBuf.byteLength),
      buf,
      BigInt(buf.byteLength),
    )) as number;
    // Raise the hint so future calls start with a large enough buffer (capped).
    if (buf.byteLength > outBufHint) {
      outBufHint = Math.min(buf.byteLength, MAX_OUT_BUF);
    }
  }

  const result = JSON.parse(dec.decode(buf.subarray(0, n))) as
    | { ok: T }
    | { err: string };

  if ("err" in result) {
    throw classifyError(result.err);
  }
  return result.ok;
}

/** Module-global hint for `nextChunk` receive buffers.  Grows up to
 * `MAX_CHUNK_BUF` (4 MB) to match the largest chunk seen; never shrinks. */
const MAX_CHUNK_BUF = 4 * 1024 * 1024; // 4 MB
let chunkBufHint = 65536;

// ── Bridge implementation ─────────────────────────────────────────────────────

export function makeBridge(endpointHandle: number) {
  // FFI handle as BigInt (handles are u64 on the Rust side; slotmap encodes
  // version bits in the upper 32 bits, so we must not truncate to u32).
  const eh = BigInt(endpointHandle);
  return {
    async nextChunk(handle: bigint): Promise<Uint8Array | null> {
      // #126: Try sync non-blocking read first — avoids ~100–200µs of
      // spawn_blocking scheduling overhead when data is already buffered.
      const syncBuf = new Uint8Array(chunkBufHint) as Uint8Array<ArrayBuffer>;
      const syncN = lib.symbols.iroh_http_try_next_chunk(
        eh,
        handle,
        syncBuf,
        BigInt(syncBuf.byteLength),
      ) as number;

      if (syncN > 0) {
        // Data available — return it without entering the async FFI pool.
        chunkBufHint = Math.min(Math.max(chunkBufHint, syncN), MAX_CHUNK_BUF);
        return syncBuf.slice(0, syncN);
      }
      if (syncN === 0) return null; // EOF
      if (syncN < -2) {
        // Buffer too small — grow and retry sync.
        const bigBuf = new Uint8Array(-syncN) as Uint8Array<ArrayBuffer>;
        const retryN = lib.symbols.iroh_http_try_next_chunk(
          eh,
          handle,
          bigBuf,
          BigInt(bigBuf.byteLength),
        ) as number;
        if (retryN > 0) {
          chunkBufHint = Math.min(
            Math.max(chunkBufHint, retryN),
            MAX_CHUNK_BUF,
          );
          return bigBuf.slice(0, retryN);
        }
        if (retryN === 0) return null;
        // Fall through to async if sync retry also fails.
      }
      // syncN === -1 (hard error) or -2 (no data yet) — fall back to async.
      if (syncN === -1) {
        throw new Error(`nextChunk: stream error on handle ${handle}`);
      }

      // -2: No data available yet — use the async path.
      // DENO-001: allocate a per-call buffer so concurrent reads on different
      // handles do not share memory and corrupt each other's data.
      let buf = new Uint8Array(chunkBufHint) as Uint8Array<ArrayBuffer>;
      let n = (await lib.symbols.iroh_http_next_chunk(
        eh,
        handle,
        buf,
        BigInt(buf.byteLength),
      )) as number;
      if (n < -1) {
        // Return value encodes the required size as a negative number.
        // Grow the buffer and retry exactly once.
        buf = new Uint8Array(-n) as Uint8Array<ArrayBuffer>;
        n = (await lib.symbols.iroh_http_next_chunk(
          eh,
          handle,
          buf,
          BigInt(buf.byteLength),
        )) as number;
      }
      // n === -1  → hard error (endpoint gone, handle invalid, stream reset).
      // n === 0   → clean EOF.
      // n > 0     → chunk of n bytes.
      if (n === -1) {
        throw new Error(`nextChunk: stream error on handle ${handle}`);
      }
      if (n === 0) return null;
      // Update hint so future calls start with a better-sized buffer (capped).
      chunkBufHint = Math.min(Math.max(chunkBufHint, n), MAX_CHUNK_BUF);
      return buf.slice(0, n);
    },
    async sendChunk(handle: bigint, chunk: Uint8Array): Promise<void> {
      // Async FFI — same deadlock concern as the class method.
      const buf = chunk as Uint8Array<ArrayBuffer>;
      const result = (await lib.symbols.iroh_http_send_chunk(
        eh,
        handle,
        buf,
        BigInt(buf.byteLength),
      )) as number;
      if (result < 0) {
        throw new Error(`sendChunk failed: handle ${handle}`);
      }
    },
    async finishBody(handle: bigint): Promise<void> {
      // #122: sync FFI — no spawn_blocking contention.
      const rc = lib.symbols.iroh_http_finish_body(
        eh,
        handle,
      ) as number;
      if (rc < 0) throw new Error(`finishBody failed: handle ${handle}`);
    },
    async cancelRequest(handle: bigint): Promise<void> {
      // #122: sync FFI — no spawn_blocking contention.
      lib.symbols.iroh_http_cancel_reader(eh, handle);
    },
    async allocFetchToken(_endpointHandle: number): Promise<bigint> {
      const res = await call<{ token: number }>("allocFetchToken", {
        endpointHandle,
      });
      return BigInt(res.token);
    },
    cancelFetch(token: bigint): void {
      // Fire-and-forget — do not await.
      void call<Record<never, never>>("cancelInFlight", {
        endpointHandle,
        token,
      });
    },
  };
}

// ── Platform functions ────────────────────────────────────────────────────────

// #126: Callback-based fetch notification — Rust calls this when a fetch
// task completes, which Deno posts to the JS event loop with µs precision
// (equivalent to Node's ThreadsafeFunction).
const fetchResolvers = new Map<bigint, () => void>();
const fetchCallback = new Deno.UnsafeCallback(
  { parameters: ["u64"], result: "void" } as const,
  (token: bigint | number) => {
    const key = BigInt(token);
    const resolve = fetchResolvers.get(key);
    if (resolve) {
      fetchResolvers.delete(key);
      resolve();
    }
  },
);

export const rawFetch: RawFetchFn = async (
  endpointHandle: number,
  nodeId: string,
  url: string,
  method: string,
  headers: [string, string][],
  reqBodyHandle: bigint | null,
  fetchToken: bigint,
  directAddrs: string[] | null,
  fetchOptions?: FetchOptions,
) => {
  // #126: Callback-based fetch — Rust notifies via UnsafeCallback when done,
  // then we call poll_fetch once to retrieve the result.  This avoids both
  // spin-polling (which blocks the JS event loop, starving same-process
  // servers) and setTimeout-based yielding (~1ms overhead per yield).
  const payloadStr = JSON.stringify(
    {
      endpointHandle,
      nodeId,
      url,
      method,
      headers,
      reqBodyHandle: reqBodyHandle != null
        ? bigintToSafeNumber(reqBodyHandle, "reqBodyHandle")
        : null,
      fetchToken: fetchToken !== 0n
        ? bigintToSafeNumber(fetchToken, "fetchToken")
        : null,
      directAddrs: directAddrs ?? null,
      timeout_ms: fetchOptions?.timeoutMs ?? null,
      decompress: fetchOptions?.decompress ?? null,
      max_response_body_bytes: fetchOptions?.maxResponseBodyBytes ?? null,
    },
    (_k, v) => (typeof v === "bigint" ? bigintToSafeNumber(v) : v),
  );
  const payloadBuf = enc.encode(payloadStr) as Uint8Array<ArrayBuffer>;

  const token = lib.symbols.iroh_http_start_fetch_cb(
    payloadBuf,
    BigInt(payloadBuf.byteLength),
    fetchCallback.pointer,
  ) as unknown as bigint;

  if (token === 0n) {
    throw classifyError("RUNTIME_INIT_FAILED: could not start fetch");
  }

  // Wait for Rust to signal completion via the callback.
  await new Promise<void>((resolve) => {
    fetchResolvers.set(token, resolve);
  });

  // Result is ready — retrieve it with a single sync poll.
  let buf = new Uint8Array(outBufHint) as Uint8Array<ArrayBuffer>;
  let n = lib.symbols.iroh_http_poll_fetch(
    token,
    buf,
    BigInt(buf.byteLength),
  ) as number;

  if (n < -1) {
    // Buffer too small — grow and retry.
    buf = new Uint8Array(-n) as Uint8Array<ArrayBuffer>;
    if (buf.byteLength > outBufHint) {
      outBufHint = Math.min(buf.byteLength, MAX_OUT_BUF);
    }
    n = lib.symbols.iroh_http_poll_fetch(
      token,
      buf,
      BigInt(buf.byteLength),
    ) as number;
  }

  if (n <= 0) {
    throw classifyError(
      "FETCH_FAILED: poll_fetch returned no data after callback",
    );
  }

  const result = JSON.parse(dec.decode(buf.subarray(0, n))) as
    | {
      ok: {
        status: number;
        headers: [string, string][];
        bodyHandle: number;
        url: string;
        inlineBody?: string;
      };
    }
    | { err: string };

  if ("err" in result) {
    throw classifyError(result.err);
  }
  const res = result.ok;
  return {
    status: res.status,
    headers: res.headers,
    bodyHandle: BigInt(res.bodyHandle),
    url: res.url,
    inlineBody: res.inlineBody ?? null,
  } satisfies FfiResponse;
};

/**
 * Synchronous respond — send the response head for a pending request via
 * the dedicated sync FFI symbol.  Never enters the async `iroh_http_call`
 * pool, so it cannot be starved by long-blocking ops like `nextChunk`.
 */
function syncRespond(
  endpointHandle: number,
  reqHandle: bigint,
  status: number,
  headers: [string, string][],
): void {
  const headersJson = enc.encode(JSON.stringify(headers));
  const rc = lib.symbols.iroh_http_respond(
    BigInt(endpointHandle),
    reqHandle,
    status,
    headersJson,
    BigInt(headersJson.byteLength),
  ) as number;
  if (rc < 0) {
    // Best-effort — don't throw, the handle may already be gone.
    // This mirrors the catch-and-ignore pattern in the old polling code.
  }
}

/**
 * Sync-poll serve loop (#122).
 *
 * Root cause of #122: `nextRequest` and `rawFetch` both go through
 * `iroh_http_call` with `nonblocking: true`, sharing Deno's fixed-size
 * `spawn_blocking` thread pool.  When 32+ `rawFetch` calls occupy all
 * threads, `nextRequest` can't get a thread → circular deadlock → 60s
 * timeout fires.
 *
 * Fix: `iroh_http_try_next_request` is a sync FFI symbol (`nonblocking: false`)
 * that does `try_recv()` on the serve queue — O(1), runs on the JS thread,
 * NEVER enters the thread pool.  JS polls with `setTimeout(0)` yield when
 * the queue is empty.  Under load the queue always has items so there's no
 * latency penalty.
 *
 * The serve lifecycle still uses `serveStart` / `stopServe` through the
 * dispatch path (they're one-shot calls, no concurrency concern).
 */
export const rawServe: RawServeFn = (
  endpointHandle: number,
  options: {
    onConnectionEvent?: (event: PeerConnectionEvent) => void;
    serveOptions?: import("@momics/iroh-http-shared").FfiServeOptions;
  },
  callback: (payload: RequestPayload) => Promise<FfiResponseHead>,
): Promise<void> => {
  // FFI handle as BigInt — endpoint handles are u64 on the Rust side.
  const eh = BigInt(endpointHandle);
  return call<Record<never, never>>("serveStart", {
    endpointHandle,
    ...options.serveOptions,
  }).then(
    () => {
      // Start connection event polling loop if a callback was supplied.
      if (options.onConnectionEvent) {
        const onEv = options.onConnectionEvent;
        (async () => {
          try {
            while (true) {
              const ev = await call<PeerConnectionEvent | null>(
                "nextConnectionEvent",
                { endpointHandle },
              );
              if (ev === null) break;
              try {
                onEv(ev);
              } catch (err) {
                console.error("[iroh-http-deno] onConnectionEvent error:", err);
              }
            }
          } catch (err) {
            if (!isEndpointGoneError(err)) throw err;
          }
        })().catch((err) => {
          if (!isEndpointGoneError(err)) {
            console.error("[iroh-http-deno] connection event loop error:", err);
          }
        });
      }

      // Track in-flight handler tasks so the loop drains them before resolving.
      const pending = new Set<Promise<void>>();

      // Per-call output buffer for try_next_request (reused across polls).
      let pollBuf = new Uint8Array(4096) as Uint8Array<ArrayBuffer>;

      return (async () => {
        const yielder = createYieldFn();
        // #115: Register cancel callback so stopServe can break the yield.
        serveCancellers.set(endpointHandle, () => yielder.cancel());
        let pollCount = 0;
        try {
          while (true) {
            // Sync poll — runs on JS thread, never enters spawn_blocking pool.
            let n = lib.symbols.iroh_http_try_next_request(
              eh,
              pollBuf,
              BigInt(pollBuf.byteLength),
            ) as number;

            pollCount++;

            if (n < -1) {
              // Buffer too small — grow and retry immediately.
              pollBuf = new Uint8Array(-n) as Uint8Array<ArrayBuffer>;
              n = lib.symbols.iroh_http_try_next_request(
                eh,
                pollBuf,
                BigInt(pollBuf.byteLength),
              ) as number;
            }

            if (n === -1) {
              // Serve stopped or queue removed.
              break;
            }

            if (n === 0) {
              // Queue empty — yield to the event loop so rawFetch results
              // and I/O can be processed, then poll again.
              // #126: MessageChannel gives ~0.017ms yields vs setTimeout(0)'s
              // ~2.5ms — critical for same-process client+server latency.
              await yielder.yield();
              continue;
            }

            // Got a request — parse and dispatch.
            const raw = JSON.parse(
              dec.decode(pollBuf.subarray(0, n)),
            );
            const payload: RequestPayload = {
              reqHandle: BigInt(raw.reqHandle),
              reqBodyHandle: BigInt(raw.reqBodyHandle),
              resBodyHandle: BigInt(raw.resBodyHandle),
              method: raw.method,
              url: raw.url,
              headers: raw.headers,
              remoteNodeId: raw.remoteNodeId,
            };

            // Handle in the background — track for drain on shutdown.
            const task: Promise<void> = (async () => {
              try {
                const head = await callback(payload);
                syncRespond(
                  endpointHandle,
                  payload.reqHandle,
                  head.status,
                  head.headers,
                );
              } catch (err) {
                console.error("[iroh-http-deno] handler error:", err);
                syncRespond(endpointHandle, payload.reqHandle, 500, []);
              }
            })();
            pending.add(task);
            task.finally(() => pending.delete(task));
          }
          await Promise.allSettled([...pending]);
        } finally {
          serveCancellers.delete(endpointHandle);
          yielder.close();
          // #245: Drain the in-flight stopServe op (if any) before resolving
          // so no non-blocking FFI op survives past `handle.finished`.
          const stopOp = serveStopOps.get(endpointHandle);
          if (stopOp) await stopOp;
        }
      })();
    },
  ).catch((err) => {
    // close_all / closeEndpoint can win the race before serveStart returns.
    if (isEndpointGoneError(err)) return;
    throw err;
  });
};

export function makeAllocBodyWriter(endpointHandle: number): AllocBodyWriterFn {
  return () =>
    call<{ handle: number }>("allocBodyWriter", { endpointHandle }).then((r) =>
      BigInt(r.handle)
    );
}

// ── Endpoint lifecycle ────────────────────────────────────────────────────────

// Register graceful-shutdown listeners once at module load time.
// Sends QUIC CONNECTION_CLOSE frames so peers observe a clean disconnect.
function _closeAllSync(): void {
  lib.symbols.iroh_http_close_all();
  // #245: close_all removes the request queues, but a serve loop parked in
  // `await yielder.yield()` only re-polls when its MessageChannel turn lands.
  // Wake every loop explicitly so it sees the -1 sentinel promptly instead of
  // relying on MessageChannel timing under load.
  for (const cancel of serveCancellers.values()) cancel();
}
Deno.addSignalListener("SIGTERM", _closeAllSync);
// SIGINT is registered only on platforms that support it (non-Windows).
if (Deno.build.os !== "windows") {
  Deno.addSignalListener("SIGINT", _closeAllSync);
}
globalThis.addEventListener("unload", _closeAllSync);

/**
 * @internal Test-only hook that triggers the same path as the SIGINT/SIGTERM
 * signal handlers. Used by the regression test for issue #155 to verify that
 * `iroh_http_close_all` wakes pending serve polling loops.
 */
export function _closeAllForTesting(): void {
  _closeAllSync();
}

/** Normalise the `discovery` option into flat fields for the Rust adapter. */
function normaliseDiscovery(
  disc?: import("@momics/iroh-http-shared").NodeOptions["discovery"],
): {
  dnsEnabled: boolean;
  dnsServerUrl?: string;
} {
  if (!disc) return { dnsEnabled: true };
  if (disc.dns === false) return { dnsEnabled: false };
  if (typeof disc.dns === "object" && disc.dns !== null) {
    return { dnsEnabled: true, dnsServerUrl: disc.dns.serverUrl };
  }
  return { dnsEnabled: true };
}

export async function createEndpointInfo(
  options?: NodeOptions,
): Promise<EndpointInfo> {
  const keyBytes: string | null = options?.key
    ? encodeBase64(
      options.key instanceof Uint8Array ? options.key : options.key.toBytes(),
    )
    : null;

  const { relayMode, relays, disableNetworking } = normaliseRelayMode(
    options?.relay,
  );
  const discovery = normaliseDiscovery(options?.discovery);
  const bindAddrs = options?.bindAddr
    ? Array.isArray(options.bindAddr) ? options.bindAddr : [options.bindAddr]
    : null;

  const res = await call<{
    endpointHandle: number;
    nodeId: string;
    keypair: number[];
  }>("createEndpoint", {
    key: keyBytes,
    idleTimeout: options?.connections?.idleTimeoutMs ?? null,
    relayMode: relayMode ?? null,
    relays: relays ?? null,
    bindAddrs,
    dnsDiscovery: discovery.dnsServerUrl ?? null,
    dnsDiscoveryEnabled: discovery.dnsEnabled,
    channelCapacity: options?.internals?.channelCapacity ?? null,
    maxChunkSizeBytes: options?.internals?.maxChunkSizeBytes ?? null,
    handleTtl: options?.internals?.handleTtl ?? null,
    maxPooledConnections: options?.connections?.maxPooled ?? null,
    poolIdleTimeoutMs: options?.connections?.poolIdleTimeoutMs ?? null,
    disableNetworking,
    proxyUrl: options?.proxy?.url ?? null,
    proxyFromEnv: options?.proxy?.fromEnv ?? null,
    keylog: options?.debug?.keylog ?? null,
    compressionLevel: typeof options?.compression === "object"
      ? (options.compression.level ?? null)
      : options?.compression
      ? 3
      : null,
    compressionMinBodyBytes: typeof options?.compression === "object"
      ? (options.compression.minBodyBytes ?? null)
      : null,
    maxHeaderBytes: options?.limits?.maxHeaderBytes ?? null,
  }).catch((e: unknown) => {
    throw classifyBindError(e);
  });
  return {
    endpointHandle: res.endpointHandle,
    nodeId: res.nodeId,
    keypair: new Uint8Array(res.keypair),
  };
}

export async function closeEndpoint(
  handle: number,
  force?: boolean,
): Promise<void> {
  await call<Record<never, never>>("closeEndpoint", {
    endpointHandle: handle,
    force: force ?? null,
  });
}

export function stopServe(handle: number): void {
  // #115: Cancel the yield immediately so the polling loop re-polls and
  // sees the -1 shutdown sentinel without waiting for the MessageChannel.
  serveCancellers.get(handle)?.();
  // #245: Register the in-flight op so the serve loop can await it before
  // resolving `finished`; otherwise this non-blocking FFI op can outlive the
  // test boundary and trip Deno's `sanitizeOps` leak detector under load.
  const op = call<Record<never, never>>("stopServe", { endpointHandle: handle })
    .catch(() => {});
  serveStopOps.set(handle, op);
  void op.finally(() => {
    if (serveStopOps.get(handle) === op) serveStopOps.delete(handle);
  });
}

/** Resolves when the endpoint has been fully closed (explicit or native). */
export function waitEndpointClosed(handle: number): Promise<void> {
  return call<Record<never, never>>("waitEndpointClosed", {
    endpointHandle: handle,
  }).then(() => {});
}

/** Snapshot of current endpoint statistics (point-in-time). */
export function endpointStats(handle: number): Promise<EndpointStats> {
  return call<EndpointStats>("endpointStats", { endpointHandle: handle });
}

// ── Address introspection ──────────────────────────────────────────────────────

export const denoAddrFns = {
  nodeAddr: async (handle: number) => {
    const res = await call<NodeAddrInfo>("nodeAddr", {
      endpointHandle: handle,
    });
    return res;
  },
  nodeTicket: async (handle: number) => {
    return call<string>("nodeTicket", { endpointHandle: handle });
  },
  homeRelay: async (handle: number) => {
    const res = await call<string | null>("homeRelay", {
      endpointHandle: handle,
    });
    return res;
  },
  peerInfo: async (handle: number, nodeId: string) => {
    const res = await call<NodeAddrInfo | null>("peerInfo", {
      endpointHandle: handle,
      nodeId,
    });
    return res;
  },
  peerStats: async (handle: number, nodeId: string) => {
    return call<PeerStats | null>("peerStats", {
      endpointHandle: handle,
      nodeId,
    });
  },
  stats: async (handle: number) => {
    return call<EndpointStats>("endpointStats", { endpointHandle: handle });
  },
};

/** Discovery functions backed by Deno FFI calls. */
export const denoDiscoveryFns = {
  mdnsBrowse: async (handle: number, serviceName: string) => {
    return call<number>("mdnsBrowse", { endpointHandle: handle, serviceName });
  },
  mdnsNextEvent: async (browseHandle: number) => {
    return call<PeerDiscoveryEvent | null>("mdnsNextEvent", { browseHandle });
  },
  mdnsBrowseClose: (browseHandle: number) => {
    call<Record<never, never>>("mdnsBrowseClose", { browseHandle }).catch(
      () => {},
    );
  },
  mdnsAdvertise: async (handle: number, serviceName: string) => {
    return call<number>("mdnsAdvertise", {
      endpointHandle: handle,
      serviceName,
    });
  },
  mdnsAdvertiseClose: (advertiseHandle: number) => {
    call<Record<never, never>>("mdnsAdvertiseClose", { advertiseHandle }).catch(
      () => {},
    );
  },
};

// ── Session functions ─────────────────────────────────────────────────────────
//
// Every session dispatch handler in dispatch.rs requires `endpointHandle` to
// look up the IrohEndpoint from the global registry.  The `RawSessionFns`
// interface passes `endpointHandle` as a parameter only for `connect`; all
// other methods only receive the session handle.  We therefore use a factory
// that closes over the endpoint handle so every call can include it.

export function makeDenoSessionFns(endpointHandle: number): RawSessionFns {
  return {
    connect: async (_endpointHandle, nodeId, directAddrs) => {
      const res = await call<{ sessionHandle: number }>("sessionConnect", {
        endpointHandle,
        nodeId,
        directAddrs: directAddrs ?? null,
      });
      return BigInt(res.sessionHandle as unknown as number);
    },
    sessionAccept: async (_endpointHandle) => {
      const res = await call<
        { sessionHandle: number; nodeId: string } | null
      >("sessionAccept", { endpointHandle });
      if (res === null) return null;
      return { sessionHandle: BigInt(res.sessionHandle), nodeId: res.nodeId };
    },
    createBidiStream: async (sessionHandle) => {
      const res = await call<{ readHandle: number; writeHandle: number }>(
        "sessionCreateBidiStream",
        { endpointHandle, sessionHandle },
      );
      return {
        readHandle: BigInt(res.readHandle),
        writeHandle: BigInt(res.writeHandle),
      } satisfies FfiDuplexStream;
    },
    nextBidiStream: async (sessionHandle) => {
      const res = await call<
        {
          readHandle: number;
          writeHandle: number;
        } | null
      >("sessionNextBidiStream", { endpointHandle, sessionHandle });
      return res
        ? ({
          readHandle: BigInt(res.readHandle),
          writeHandle: BigInt(res.writeHandle),
        } satisfies FfiDuplexStream)
        : null;
    },
    createUniStream: async (sessionHandle) => {
      const res = await call<{ writeHandle: number }>(
        "sessionCreateUniStream",
        {
          endpointHandle,
          sessionHandle,
        },
      );
      return BigInt(res.writeHandle);
    },
    nextUniStream: async (sessionHandle) => {
      const res = await call<{ readHandle: number } | null>(
        "sessionNextUniStream",
        { endpointHandle, sessionHandle },
      );
      return res ? BigInt(res.readHandle) : null;
    },
    sendDatagram: async (sessionHandle, data) => {
      await call<Record<never, never>>("sessionSendDatagram", {
        endpointHandle,
        sessionHandle,
        data: encodeBase64(data),
      });
    },
    recvDatagram: async (sessionHandle) => {
      const res = await call<{ data: string } | null>("sessionRecvDatagram", {
        endpointHandle,
        sessionHandle,
      });
      return res ? decodeBase64(res.data) : null;
    },
    maxDatagramSize: async (sessionHandle) => {
      const res = await call<{ maxDatagramSize: number | null }>(
        "sessionMaxDatagramSize",
        { endpointHandle, sessionHandle },
      );
      return res.maxDatagramSize;
    },
    closed: async (sessionHandle) => {
      return call<{ closeCode: number; reason: string }>("sessionClosed", {
        endpointHandle,
        sessionHandle,
      });
    },
    close: async (sessionHandle, closeCode?, reason?) => {
      await call<Record<never, never>>("sessionClose", {
        endpointHandle,
        sessionHandle,
        closeCode,
        reason,
      });
    },
  };
}

// ── Cryptography ───────────────────────────────────────────────────────────────

/**
 * Sign `data` with a 32-byte Ed25519 secret key.
 * Returns a 64-byte signature.
 */
export async function secretKeySign(
  secretKey: Uint8Array,
  data: Uint8Array,
): Promise<Uint8Array> {
  const b64 = await call<string>("secretKeySign", {
    secretKey: encodeBase64(secretKey),
    data: encodeBase64(data),
  });
  return decodeBase64(b64);
}

/**
 * Verify an Ed25519 signature.
 * @param publicKey  32-byte Ed25519 public key.
 * @param data       Original message bytes.
 * @param signature  64-byte signature to verify.
 * @returns `true` if the signature is valid.
 */
export async function publicKeyVerify(
  publicKey: Uint8Array,
  data: Uint8Array,
  signature: Uint8Array,
): Promise<boolean> {
  return call<boolean>("publicKeyVerify", {
    publicKey: encodeBase64(publicKey),
    data: encodeBase64(data),
    signature: encodeBase64(signature),
  });
}

/**
 * Generate a fresh random 32-byte Ed25519 secret key.
 */
export async function generateSecretKey(): Promise<Uint8Array> {
  const b64 = await call<string>("generateSecretKey", {});
  return decodeBase64(b64);
}

// ── DenoAdapter ───────────────────────────────────────────────────────────────

/**
 * Concrete IrohAdapter backed by Deno FFI.
 * One instance per endpoint — constructed with the endpoint handle.
 */
export class DenoAdapter extends IrohAdapter {
  readonly #eh: number;
  readonly #sessionFnsInstance: RawSessionFns;

  constructor(endpointHandle: number) {
    super();
    this.#eh = endpointHandle;
    this.#sessionFnsInstance = makeDenoSessionFns(endpointHandle);
  }

  // ── Body streaming ──────────────────────────────────────────────────────────

  async nextChunk(handle: bigint): Promise<Uint8Array | null> {
    const eh = BigInt(this.#eh);
    // #126: Try sync non-blocking read first — avoids ~100–200µs of
    // spawn_blocking scheduling overhead when data is already buffered.
    const syncBuf = new Uint8Array(chunkBufHint) as Uint8Array<ArrayBuffer>;
    const syncN = lib.symbols.iroh_http_try_next_chunk(
      eh,
      handle,
      syncBuf,
      BigInt(syncBuf.byteLength),
    ) as number;

    if (syncN > 0) {
      chunkBufHint = Math.min(Math.max(chunkBufHint, syncN), MAX_CHUNK_BUF);
      return syncBuf.slice(0, syncN);
    }
    if (syncN === 0) return null; // EOF
    if (syncN < -2) {
      // Buffer too small — grow and retry sync.
      const bigBuf = new Uint8Array(-syncN) as Uint8Array<ArrayBuffer>;
      const retryN = lib.symbols.iroh_http_try_next_chunk(
        eh,
        handle,
        bigBuf,
        BigInt(bigBuf.byteLength),
      ) as number;
      if (retryN > 0) {
        chunkBufHint = Math.min(Math.max(chunkBufHint, retryN), MAX_CHUNK_BUF);
        return bigBuf.slice(0, retryN);
      }
      if (retryN === 0) return null;
    }
    if (syncN === -1) {
      throw new Error(`nextChunk: stream error on handle ${handle}`);
    }

    // -2: No data yet — fall back to async path.
    let buf = new Uint8Array(chunkBufHint) as Uint8Array<ArrayBuffer>;
    let n = (await lib.symbols.iroh_http_next_chunk(
      eh,
      handle,
      buf,
      BigInt(buf.byteLength),
    )) as number;
    if (n < -1) {
      buf = new Uint8Array(-n) as Uint8Array<ArrayBuffer>;
      n = (await lib.symbols.iroh_http_next_chunk(
        eh,
        handle,
        buf,
        BigInt(buf.byteLength),
      )) as number;
    }
    if (n === -1) {
      throw new Error(`nextChunk: stream error on handle ${handle}`);
    }
    if (n === 0) return null;
    chunkBufHint = Math.min(Math.max(chunkBufHint, n), MAX_CHUNK_BUF);
    return buf.slice(0, n);
  }

  async sendChunk(handle: bigint, chunk: Uint8Array): Promise<void> {
    // Async FFI (nonblocking:true) — avoids blocking the JS event loop when
    // the bounded channel is full.  Sync would deadlock same-process
    // client+server on large bodies.
    const buf = chunk as Uint8Array<ArrayBuffer>;
    const result = (await lib.symbols.iroh_http_send_chunk(
      BigInt(this.#eh),
      handle,
      buf,
      BigInt(buf.byteLength),
    )) as number;
    if (result < 0) throw new Error(`sendChunk failed: handle ${handle}`);
  }

  async finishBody(handle: bigint): Promise<void> {
    // #122: sync FFI — no spawn_blocking contention.
    const rc = lib.symbols.iroh_http_finish_body(
      BigInt(this.#eh),
      handle,
    ) as number;
    if (rc < 0) throw new Error(`finishBody failed: handle ${handle}`);
  }

  async cancelRequest(handle: bigint): Promise<void> {
    // #122: sync FFI — no spawn_blocking contention.
    lib.symbols.iroh_http_cancel_reader(BigInt(this.#eh), handle);
  }

  async allocFetchToken(_endpointHandle: number): Promise<bigint> {
    const res = await call<{ token: number }>("allocFetchToken", {
      endpointHandle: this.#eh,
    });
    return BigInt(res.token);
  }

  cancelFetch(token: bigint): void {
    void call<Record<never, never>>("cancelInFlight", {
      endpointHandle: this.#eh,
      token,
    });
  }

  allocBodyWriter(_endpointHandle: number): Promise<bigint> {
    return call<{ handle: number }>("allocBodyWriter", {
      endpointHandle: this.#eh,
    }).then((r) => BigInt(r.handle));
  }

  // ── Raw transport ───────────────────────────────────────────────────────────

  rawFetch(
    endpointHandle: number,
    nodeId: string,
    url: string,
    method: string,
    headers: [string, string][],
    reqBodyHandle: bigint | null,
    fetchToken: bigint,
    directAddrs: string[] | null,
    fetchOptions?: FetchOptions,
  ): Promise<FfiResponse> {
    // Delegate to the module-level rawFetch which uses the split-fetch
    // pattern (#126) — sync start + sync poll — bypassing spawn_blocking.
    return rawFetch(
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
  }

  rawServe(
    endpointHandle: number,
    options: {
      onConnectionEvent?: (event: PeerConnectionEvent) => void;
      serveOptions?: import("@momics/iroh-http-shared").FfiServeOptions;
    },
    callback: (payload: RequestPayload) => Promise<FfiResponseHead>,
  ): Promise<void> {
    // Delegate to the module-level rawServe which owns the polling loop.
    return rawServe(endpointHandle, options, callback);
  }

  // ── Lifecycle ───────────────────────────────────────────────────────────────

  async closeEndpoint(handle: number, force?: boolean): Promise<void> {
    await call<Record<never, never>>("closeEndpoint", {
      endpointHandle: handle,
      force: force ?? null,
    });
  }

  stopServe(handle: number): void {
    // Delegate to the module-level stopServe so the in-flight op is tracked
    // and drained by the serve loop (#115/#245).
    stopServe(handle);
  }

  waitEndpointClosed(handle: number): Promise<void> {
    return call<Record<never, never>>("waitEndpointClosed", {
      endpointHandle: handle,
    }).then(() => {});
  }

  // ── Address / stats ─────────────────────────────────────────────────────────

  async nodeAddr(handle: number): Promise<NodeAddrInfo> {
    return call<NodeAddrInfo>("nodeAddr", { endpointHandle: handle });
  }

  async nodeTicket(handle: number): Promise<string> {
    return call<string>("nodeTicket", { endpointHandle: handle });
  }

  async homeRelay(handle: number): Promise<string | null> {
    return call<string | null>("homeRelay", { endpointHandle: handle });
  }

  async peerInfo(handle: number, nodeId: string): Promise<NodeAddrInfo | null> {
    return call<NodeAddrInfo | null>("peerInfo", {
      endpointHandle: handle,
      nodeId,
    });
  }

  async peerStats(handle: number, nodeId: string): Promise<PeerStats | null> {
    return call<PeerStats | null>("peerStats", {
      endpointHandle: handle,
      nodeId,
    });
  }

  async stats(handle: number): Promise<EndpointStats> {
    return call<EndpointStats>("endpointStats", { endpointHandle: handle });
  }

  // ── mDNS discovery ──────────────────────────────────────────────────────────

  override mdnsBrowse(
    endpointHandle: number,
    serviceName: string,
  ): Promise<number> {
    return call<number>("mdnsBrowse", { endpointHandle, serviceName });
  }

  override mdnsNextEvent(
    browseHandle: number,
  ): Promise<PeerDiscoveryEvent | null> {
    return call<PeerDiscoveryEvent | null>("mdnsNextEvent", { browseHandle });
  }

  override mdnsBrowseClose(browseHandle: number): void {
    call<Record<never, never>>("mdnsBrowseClose", { browseHandle }).catch(
      () => {},
    );
  }

  override mdnsAdvertise(
    endpointHandle: number,
    serviceName: string,
  ): Promise<number> {
    return call<number>("mdnsAdvertise", { endpointHandle, serviceName });
  }

  override mdnsAdvertiseClose(advertiseHandle: number): void {
    call<Record<never, never>>("mdnsAdvertiseClose", { advertiseHandle }).catch(
      () => {},
    );
  }

  // ── Sessions ────────────────────────────────────────────────────────────────

  override get sessionFns(): RawSessionFns {
    return this.#sessionFnsInstance;
  }

  // ── Transport events ────────────────────────────────────────────────────────

  #transportLoopDone: Promise<void> | null = null;

  override startTransportEvents(
    _endpointHandle: number,
    callback: (event: TransportEventPayload) => void,
  ): void {
    // Claim the receiver on the Rust side, then drain in the background.
    // Rust returns null from startTransportEvents if the endpoint is already
    // closed, and null from nextTransportEvent when the channel closes.
    // Both are clean shutdown signals — the loop resolves without error.
    this.#transportLoopDone = (async () => {
      try {
        const subscribed = await call<null | boolean>("startTransportEvents", {
          endpointHandle: this.#eh,
        });
        if (subscribed === false) return; // endpoint gone before subscribe
        while (true) {
          const event = await call<TransportEventPayload | null>(
            "nextTransportEvent",
            { endpointHandle: this.#eh },
          );
          if (event === null) break;
          try {
            callback(event);
          } catch (err) {
            console.error(
              "[iroh-http-deno] transport event callback error:",
              err,
            );
          }
        }
      } catch (err) {
        if (isEndpointGoneError(err)) return;
        throw err;
      }
    })();
    this.#transportLoopDone.catch((err: unknown) => {
      if (isEndpointGoneError(err)) return;
      console.error("[iroh-http-deno] startTransportEvents error:", err);
    });
  }

  override drainTransportEvents(): Promise<void> {
    return this.#transportLoopDone ?? Promise.resolve();
  }

  override async nextPathChange(
    _endpointHandle: number,
    nodeId: string,
  ): Promise<PathInfo | null> {
    return call<PathInfo | null>("nextPathChange", {
      endpointHandle: this.#eh,
      nodeId,
    });
  }
}
