import type { SecretKey } from "../SecretKey.js";

/**
 * Named relay mode for the Iroh relay network.
 *
 * - `"default"` — use Iroh's public relay servers (recommended).
 * - `"staging"` — use Iroh's staging relay servers (testing only).
 * - `"disabled"` — direct QUIC only; connections fail if hole-punching is not possible.
 * - Any other string — treated as a single custom relay server URL (see `relay.urls`).
 *
 * Use `relay.urls` when you need multiple custom relay URLs.
 */
export type RelayMode = "default" | "staging" | "disabled" | (string & {});

export interface NodeOptions {
  /**
   * Ed25519 identity for this node. Accepts a `SecretKey` instance or raw
   * 32-byte key material as a `Uint8Array`. When omitted a fresh key is generated.
   */
  key?: SecretKey | Uint8Array;

  /** Relay server configuration for NAT traversal. */
  relay?: {
    /**
     * Which relay network to use.
     * @default "default"
     */
    mode?: RelayMode;
    /**
     * One or more custom relay server URLs. When set, `mode` is ignored and
     * these URLs are used exclusively as the relay server list.
     */
    urls?: string[];
  };

  /**
   * Local socket address(es) the QUIC endpoint binds to.
   * Use `"0.0.0.0:0"` (or `"[::]:0"`) to let the OS pick a port.
   * @default "0.0.0.0:0"
   */
  bindAddr?: string | string[];

  /** Peer discovery backends. */
  discovery?: {
    /**
     * DNS-SD peer discovery. Pass `true` to use the default Iroh DNS server,
     * or `{ serverUrl }` to point at a custom DNS-SD resolver.
     */
    dns?: boolean | { serverUrl?: string };
    /**
     * mDNS peer discovery on the local network. Pass `true` to use the default
     * service name (`"iroh-http"`), or `{ serviceName }` for a custom name.
     */
    mdns?: boolean | { serviceName?: string };
  };

  /** HTTP proxy for outbound relay connections. */
  proxy?: {
    /**
     * Proxy URL, e.g. `"http://proxy:8080"`. Takes precedence over `fromEnv`.
     */
    url?: string;
    /**
     * When `true`, read proxy settings from `HTTP_PROXY` / `HTTPS_PROXY`
     * environment variables. Ignored when `url` is set.
     */
    fromEnv?: boolean;
  };

  /** Connection pool tuning. */
  connections?: {
    /**
     * Maximum QUIC connections kept alive in the pool. Older idle connections
     * are evicted when the limit is hit.
     * @default 512
     */
    maxPooled?: number;
    /**
     * QUIC idle timeout in milliseconds. Connections with no activity for this
     * duration are closed. Set to `0` to disable.
     * @default 30_000
     */
    idleTimeoutMs?: number;
    /**
     * Milliseconds before an idle pooled connection is dropped. Omit to keep
     * pooled connections until capacity eviction or transport failure.
     * @default undefined
     */
    poolIdleTimeoutMs?: number;
  };

  /** Request header limits. */
  limits?: {
    /**
     * Maximum total size of all request headers in bytes. Requests that exceed
     * this limit receive `431 Request Header Fields Too Large`.
     * @default 65_536 (64 KB)
     */
    maxHeaderBytes?: number;
  };

  /**
   * Enable zstd response compression. Pass `true` to use defaults, or an
   * object to tune the compression level and minimum body size.
   * @default false
   */
  compression?: boolean | { level?: number; minBodyBytes?: number };

  /** Automatic reconnect policy for outbound connections. */
  reconnect?: {
    /** Enable automatic reconnect on connection loss. @default false */
    auto?: boolean;
    /** Maximum reconnect attempts before giving up. @default 3 */
    maxRetries?: number;
    /**
     * Called (Tauri adapter only) when a foreground transport probe finds the
     * connection dead, so the application can recreate the node — e.g.
     * `createNode({ key })` with the same identity — instead of the node being
     * closed. When omitted, an unrecoverable transport closes the node so its
     * `closed` promise resolves and the app can react. Honored only when the
     * reconnect listener is enabled (see `auto`).
     */
    onReconnectNeeded?: () => void | Promise<void>;
    /**
     * Called (Tauri adapter only) when `onReconnectNeeded` throws/rejects, or
     * when the fail-safe close of the unusable node fails. If both operations
     * fail, this callback is invoked twice in that order. When omitted,
     * failures are reported to `console.error`.
     */
    onReconnectError?: (error: unknown) => void;
  };

  /**
   * When `true`, the endpoint binds only to loopback. Useful for tests that
   * must not touch the network.
   * @default false
   */
  disableNetworking?: boolean;

  /** Debugging tools. Never enable these in production. */
  debug?: {
    /**
     * Write TLS session keys to a keylog file for Wireshark capture.
     * Reads the `SSLKEYLOGFILE` environment variable for the file path.
     */
    keylog?: boolean;
  };

  /**
   * Low-level Rust runtime knobs. Only touch these if you understand the
   * internal request pipeline.
   */
  internals?: {
    /**
     * Capacity of the internal mpsc channel that queues requests from the QUIC
     * accept loop to the JS handler. Increase if you see dropped requests under
     * burst load.
     * @default 256
     */
    channelCapacity?: number;
    /**
     * Maximum body chunk size in bytes for streaming request/response bodies
     * over the FFI boundary.
     * @default 65_536 (64 KB)
     */
    maxChunkSizeBytes?: number;
    /**
     * Milliseconds before an idle body or request handle is evicted from the
     * slab. Prevents handle leaks when callers fail to consume or cancel a body.
     * @default 60_000
     */
    handleTtl?: number;
  };
}
