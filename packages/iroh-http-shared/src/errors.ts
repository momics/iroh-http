/**
 * Structured error classes for iroh-http.
 *
 * All errors from the Rust/FFI layer are classified through `classifyError`
 * before reaching user code.  This gives callers `instanceof` checks and
 * machine-readable `.code` strings.
 */

// ── Base class ────────────────────────────────────────────────────────────────

/**
 * Base class for all iroh-http errors.
 *
 * Every error from the Rust/FFI layer is an `IrohError` or subclass, providing
 * a machine-readable `.code` string for programmatic handling.
 *
 * The `.name` property follows `DOMException` naming conventions where a
 * direct analogue exists, so existing web-platform error-handling patterns
 * work without modification:
 *
 * | Subclass            | `.name`        | DOMException equivalent     |
 * |---------------------|----------------|-----------------------------|
 * | IrohAbortError      | "AbortError"   | DOMException AbortError     |
 * | IrohConnectError    | "NetworkError" | DOMException NetworkError   |
 * | IrohBindError       | "NetworkError" | DOMException NetworkError   |
 * | IrohArgumentError   | "TypeError"    | DOMException TypeError      |
 * | IrohStreamError     | "IrohStreamError"  | (no direct analogue)    |
 * | IrohProtocolError   | "IrohProtocolError" | (no direct analogue)   |
 *
 * @example
 * ```ts
 * try {
 *   await node.fetch(peer.toURL('/api'));
 * } catch (e) {
 *   if (e instanceof IrohError) {
 *     console.error(`[${e.code}] ${e.message}`);
 *   }
 *   // Web-platform pattern also works:
 *   if (e.name === "NetworkError") { /* peer unreachable *\/ }
 *   if (e.name === "AbortError")   { /* user cancelled  *\/ }
 * }
 * ```
 */
export class IrohError extends Error {
  /** Machine-readable error code string (e.g. `"TIMEOUT"`, `"INVALID_HANDLE"`). */
  readonly code: string;

  constructor(message: string, code: string) {
    super(message);
    this.name = "IrohError";
    this.code = code;
    // Restore prototype chain in transpiled environments.
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

// ── Subclasses ────────────────────────────────────────────────────────────────

/**
 * Failed to bind or create an Iroh endpoint.
 *
 * Thrown by `createNode()` when the QUIC endpoint cannot be created —
 * for example, the UDP port is already in use or the secret key is invalid.
 *
 * @example
 * ```ts
 * try {
 *   const node = await createNode({ bindAddr: '0.0.0.0:4433' });
 * } catch (e) {
 *   if (e instanceof IrohBindError) {
 *     console.error('Bind failed:', e.code, e.message);
 *   }
 * }
 * ```
 */
export class IrohBindError extends IrohError {
  constructor(message: string, code: string) {
    super(message, code);
    this.name = "NetworkError";
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/**
 * Failed to connect to a remote peer.
 *
 * Covers DNS resolution, timeout, connection refused, and ALPN mismatch errors.
 *
 * @example
 * ```ts
 * try {
 *   const res = await node.fetch(peerId.toURL('/api'));
 * } catch (e) {
 *   if (e instanceof IrohConnectError && e.code === 'TIMEOUT') {
 *     console.error('Peer unreachable — try again later');
 *   }
 * }
 * ```
 */
export class IrohConnectError extends IrohError {
  constructor(message: string, code: string) {
    super(message, code);
    this.name = "NetworkError";
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/** A body read or write stream failed mid-transfer (reset, timeout, or cancelled). */
export class IrohStreamError extends IrohError {
  constructor(message: string, code: string) {
    super(message, code);
    this.name = "IrohStreamError";
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/** HTTP framing / protocol error — the peer sent malformed headers or rejected an upgrade. */
export class IrohProtocolError extends IrohError {
  constructor(message: string, code: string) {
    super(message, code);
    this.name = "IrohProtocolError";
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/** The operation was aborted via `AbortSignal`.  Mirrors the web platform `AbortError`. */
export class IrohAbortError extends IrohError {
  constructor(message = "The operation was aborted") {
    super(message, "ABORTED");
    this.name = "AbortError";
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/** Invalid argument passed by the caller. */
export class IrohArgumentError extends IrohError {
  constructor(message: string, code = "INVALID_ARGUMENT") {
    super(message, code);
    this.name = "TypeError";
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/** Slab handle is invalid or expired. */
export class IrohHandleError extends IrohStreamError {
  constructor(message: string, code = "INVALID_HANDLE") {
    super(message, code);
    this.name = "IrohHandleError";
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

// ── Classification ────────────────────────────────────────────────────────────

/**
 * Map a raw Rust error string to the appropriate structured error class.
 *
 * The Rust layer uses stable, prefixed error messages (e.g. `"connect: …"`,
 * `"parse response head: …"`).  This function uses those prefixes plus a few
 * keyword tests to pick the right subclass and code.
 *
 * Long-term the Rust side should emit structured `{ code, message }` JSON,
 * but string-prefix matching is sufficient while the error messages are
 * under our control.
 */
export function classifyError(raw: string | unknown): IrohError {
  // Pass through already-classified errors unchanged.
  if (raw instanceof IrohError) return raw;

  let msg: string;
  let code: string | null = null;

  if (typeof raw === "string") {
    // Try to detect structured `{"code":"...","message":"..."}` JSON emitted
    // by `iroh_http_core::classify_error_json`.
    if (raw.startsWith("{")) {
      try {
        const parsed = JSON.parse(raw) as Record<string, unknown>;
        if (
          typeof parsed.code === "string" && typeof parsed.message === "string"
        ) {
          code = parsed.code;
          msg = parsed.message;
        } else {
          msg = raw;
        }
      } catch {
        msg = raw;
      }
    } else {
      msg = raw;
    }
  } else if (raw instanceof Error) {
    // napi/Tauri may wrap the JSON string inside an Error.message.
    const m = raw.message;
    if (m.startsWith("{")) {
      try {
        const parsed = JSON.parse(m) as Record<string, unknown>;
        if (
          typeof parsed.code === "string" && typeof parsed.message === "string"
        ) {
          code = parsed.code;
          msg = parsed.message;
        } else {
          msg = m;
        }
      } catch {
        msg = m;
      }
    } else {
      msg = m;
    }
  } else {
    msg = String(raw);
  }

  if (code) {
    return classifyByCode(code, msg);
  }

  // All errors from adapters use structured JSON codes. Unknown strings fall back.
  return new IrohError(msg, "UNKNOWN");
}

function classifyByCode(code: string, msg: string): IrohError {
  switch (code) {
    // ── Connection errors ──────────────────────────────────────────────────
    case "TIMEOUT":
      return new IrohConnectError(msg, code);
    case "DNS_FAILURE":
      return new IrohConnectError(msg, code);
    case "ALPN_MISMATCH":
      return new IrohConnectError(msg, code);
    case "REFUSED":
      return new IrohConnectError(msg, code);
    case "PEER_REJECTED":
      return new IrohConnectError(msg, code);

    // ── Protocol errors ────────────────────────────────────────────────────
    case "UPGRADE_REJECTED":
      return new IrohProtocolError(msg, code);
    case "PARSE_FAILURE":
      return new IrohProtocolError(msg, code);
    case "TOO_MANY_HEADERS":
      return new IrohProtocolError(msg, code);
    case "BODY_TOO_LARGE":
      return new IrohProtocolError(msg, code);
    case "HEADER_TOO_LARGE":
      return new IrohProtocolError(msg, code);

    // ── Handle / stream errors ─────────────────────────────────────────────
    case "INVALID_HANDLE":
      return new IrohHandleError(msg, code);
    case "WRITER_DROPPED":
      return new IrohStreamError(msg, code);
    case "READER_DROPPED":
      return new IrohStreamError(msg, code);
    case "STREAM_RESET":
      return new IrohStreamError(msg, code);

    // ── Bind / endpoint errors ─────────────────────────────────────────────
    case "INVALID_KEY":
      return new IrohBindError(msg, code);
    case "ENDPOINT_FAILURE":
      return new IrohBindError(msg, code);

    // ── Abort / cancel ─────────────────────────────────────────────────────
    case "ABORTED":
    case "CANCELLED":
      return new IrohAbortError(msg);

    // ── Input validation ───────────────────────────────────────────────────
    case "INVALID_ARGUMENT":
    case "INVALID_INPUT":
      return new IrohArgumentError(msg, code);

    default:
      return new IrohError(msg, code);
  }
}

/**
 * Classify a bind/create error specifically.
 * Used by platform adapters for `createEndpoint` rejections.
 */
export function classifyBindError(raw: string | unknown): IrohBindError {
  // If already a structured error, promote to IrohBindError preserving the code.
  const classified = classifyError(raw);
  if (classified instanceof IrohBindError) return classified;
  // Re-wrap with bind context so callers always get IrohBindError.
  return new IrohBindError(classified.message, classified.code);
}

/** True when an FFI call failed because the endpoint was already torn down. */
export function isEndpointGoneError(err: unknown): boolean {
  if (err instanceof IrohHandleError) return true;
  return err instanceof IrohError && err.code === "INVALID_HANDLE";
}
