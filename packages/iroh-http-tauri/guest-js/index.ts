/**
 * iroh-http-tauri — guest-js entry point.
 */

import { Channel, invoke } from "@tauri-apps/api/core";
import { installForegroundHealthCheck } from "./lifecycle.js";
import {
  bigintToSafeNumber,
  classifyBindError,
  encodeBase64,
  IrohNode,
  type IrohNodeWithSecret,
  type NodeOptions,
  normaliseCompression,
  normaliseDiscovery,
  normaliseRelayMode,
  type SecretKey,
} from "@momics/iroh-http-shared";
import {
  type FfiDuplexStream,
  type FfiResponse,
  type FfiResponseHead,
  IrohAdapter,
  type PeerConnectionEvent,
  type RequestPayload,
  type TransportEventPayload,
} from "@momics/iroh-http-shared/adapter";
import type { RawSessionFns } from "@momics/iroh-http-shared/adapter";
import type {
  DiscoveryInfo,
  DnsSdProtocol,
  EndpointStats,
  NodeAddrInfo,
  PathInfo,
  PeerDiscoveryEvent,
  PeerStats,
  ServiceConfig,
  ServiceRecord,
} from "@momics/iroh-http-shared";

const PLUGIN = "plugin:iroh-http";

/**
 * #338: Detect the Rust `send_chunk` rejection that means the platform WebView
 * did not deliver our raw binary invoke payload as binary (the Android System
 * WebView turns it into JSON). The command signals this with INVALID_INPUT and
 * the message "expected raw binary body or base64 string"; on that specific
 * error we retry the chunk base64-encoded via the command's JSON compat path.
 */
function isRawBinaryUnsupported(err: unknown): boolean {
  const msg = err instanceof Error ? err.message : String(err);
  return msg.includes("raw binary body or base64");
}

/** Tauri-specific request payload shape (camelCase, serialised from Rust). */
interface TauriRequestPayload {
  reqHandle: number;
  reqBodyHandle: number;
  resBodyHandle: number;
  method: string;
  url: string;
  headers: string[][];
  remoteNodeId: string;
}

// ── TauriAdapter ──────────────────────────────────────────────────────────────

class TauriAdapter extends IrohAdapter {
  #epHandle: number;
  #sessionFnsCache: RawSessionFns;
  #serveStopRequested = false;
  // #338: How `sendChunk` encodes body bytes over the Tauri IPC channel. Starts
  // on the fast raw-binary path and sticks to base64 once a platform WebView is
  // found to reject raw payloads (the Android System WebView). Per-adapter, so
  // desktop / iOS nodes are never affected.
  #sendChunkMode: "raw" | "base64" = "raw";

  constructor(epHandle: number) {
    super();
    this.#epHandle = epHandle;
    this.#sessionFnsCache = makeTauriSessionFns(epHandle);
  }

  nextChunk(handle: bigint): Promise<Uint8Array | null> {
    // Sync fast path: if data is already buffered, skip async IPC overhead.
    // Protocol: first byte is 0x01 (data follows) or 0x00 (EOF).
    const safeHandle = bigintToSafeNumber(handle);
    return invoke<ArrayBuffer>(`${PLUGIN}|try_next_chunk`, {
      endpointHandle: this.#epHandle,
      handle: safeHandle,
    }).then(
      (buf) => {
        const v = new Uint8Array(buf);
        return v.length > 0 && v[0] !== 0 ? v.subarray(1) : null;
      },
      // Channel empty or lock contended — fall back to async.
      () =>
        invoke<ArrayBuffer>(`${PLUGIN}|next_chunk`, {
          endpointHandle: this.#epHandle,
          handle: safeHandle,
        }).then((buf) => {
          const v = new Uint8Array(buf);
          return v.length > 0 && v[0] !== 0 ? v.subarray(1) : null;
        }),
    );
  }

  async sendChunk(handle: bigint, chunk: Uint8Array): Promise<void> {
    const headers = {
      "iroh-ep": String(this.#epHandle),
      "iroh-handle": String(bigintToSafeNumber(handle)),
    };
    // #338: The Android System WebView does not transmit a raw `Uint8Array`
    // invoke payload as `InvokeBody::Raw` — it arrives as JSON and the Rust
    // `send_chunk` command rejects it with INVALID_INPUT ("expected raw binary
    // body or base64 string"). That command also accepts a base64 string via its
    // JSON compatibility path. Desktop and iOS (WKWebView) deliver raw binary
    // fine, so we stay on the fast raw path there and only switch — permanently,
    // for this adapter — after the platform proves it can't handle raw binary.
    // `pipeToWriter` awaits one `sendChunk` at a time per body, so switching
    // mid-stream never reorders chunks.
    if (this.#sendChunkMode === "base64") {
      await this.#sendChunkBase64(chunk, headers);
      return;
    }
    try {
      await invoke(`${PLUGIN}|send_chunk`, chunk, { headers });
    } catch (err) {
      if (!isRawBinaryUnsupported(err)) throw err;
      this.#sendChunkMode = "base64";
      await this.#sendChunkBase64(chunk, headers);
    }
  }

  /**
   * Send a body chunk over the `send_chunk` command's base64 JSON compat path
   * (the Rust side reads it via `val.as_str()`). Tauri's `InvokeArgs` type does
   * not include a bare string, but a string payload serialises to a JSON string
   * at runtime — exactly what the command expects — so the cast is required and
   * safe.
   */
  #sendChunkBase64(
    chunk: Uint8Array,
    headers: Record<string, string>,
  ): Promise<void> {
    return invoke(
      `${PLUGIN}|send_chunk`,
      encodeBase64(chunk) as unknown as Parameters<typeof invoke>[1],
      { headers },
    );
  }

  finishBody(handle: bigint): Promise<void> {
    return invoke(`${PLUGIN}|finish_body`, {
      endpointHandle: this.#epHandle,
      handle: bigintToSafeNumber(handle),
    });
  }

  cancelRequest(handle: bigint): Promise<void> {
    return invoke(`${PLUGIN}|cancel_request`, {
      endpointHandle: this.#epHandle,
      handle: bigintToSafeNumber(handle),
    });
  }

  allocFetchToken(endpointHandle: number): Promise<bigint> {
    return invoke<number>(`${PLUGIN}|create_fetch_token`, { endpointHandle })
      .then(BigInt);
  }

  cancelFetch(token: bigint): void {
    void invoke(`${PLUGIN}|cancel_in_flight`, {
      endpointHandle: this.#epHandle,
      token: bigintToSafeNumber(token, "token"),
    });
  }

  allocBodyWriter(_endpointHandle: number): Promise<bigint> {
    return invoke<number>(`${PLUGIN}|create_body_writer`, {
      endpointHandle: this.#epHandle,
    }).then(BigInt);
  }

  async rawFetch(
    endpointHandle: number,
    nodeId: string,
    url: string,
    method: string,
    headers: [string, string][],
    reqBodyHandle: bigint | null,
    fetchToken: bigint,
    directAddrs: string[] | null,
    fetchOptions?: import("@momics/iroh-http-shared").FetchOptions,
  ): Promise<FfiResponse> {
    const res = await invoke<{
      status: number;
      headers: string[][];
      bodyHandle: number;
      url: string;
    }>(`${PLUGIN}|fetch`, {
      args: {
        endpointHandle: Number(endpointHandle),
        nodeId,
        url,
        method,
        headers,
        reqBodyHandle: reqBodyHandle != null
          ? bigintToSafeNumber(reqBodyHandle, "reqBodyHandle")
          : null,
        fetchToken: fetchToken != null
          ? bigintToSafeNumber(fetchToken, "fetchToken")
          : null,
        directAddrs: directAddrs ?? null,
        timeoutMs: fetchOptions?.timeoutMs ?? null,
        decompress: fetchOptions?.decompress ?? null,
        maxResponseBodyBytes: fetchOptions?.maxResponseBodyBytes ?? null,
      },
    });
    return {
      status: res.status,
      headers: res.headers as [string, string][],
      bodyHandle: BigInt(res.bodyHandle),
      url: res.url,
    } satisfies FfiResponse;
  }

  rawServe(
    endpointHandle: number,
    options: {
      onConnectionEvent?: (event: PeerConnectionEvent) => void;
      serveOptions?: import("@momics/iroh-http-shared").FfiServeOptions;
    },
    callback: (payload: RequestPayload) => Promise<FfiResponseHead>,
  ): Promise<void> {
    this.#serveStopRequested = false;
    const channel = new Channel<TauriRequestPayload>();

    let connChannel: Channel<PeerConnectionEvent> | undefined;
    if (options.onConnectionEvent) {
      const cb = options.onConnectionEvent;
      connChannel = new Channel<PeerConnectionEvent>();
      connChannel.onmessage = (ev: PeerConnectionEvent) => cb(ev);
    } else {
      connChannel = new Channel<PeerConnectionEvent>();
    }

    // Track in-flight handler tasks so shutdown drains them before resolving,
    // matching the Node/Deno serve contract. The Rust dispatch callback is
    // fire-and-forget (it only sends the request event onto the channel and
    // returns), so `wait_serve_stop()` alone does not guarantee JS handler
    // promises — and their follow-up `respond_to_request` IPC — have settled.
    const pending = new Set<Promise<void>>();

    channel.onmessage = (raw: TauriRequestPayload) => {
      const task = (async () => {
        const payload: RequestPayload = {
          reqHandle: BigInt(raw.reqHandle),
          reqBodyHandle: BigInt(raw.reqBodyHandle),
          resBodyHandle: BigInt(raw.resBodyHandle),
          method: raw.method,
          url: raw.url,
          headers: raw.headers as [string, string][],
          remoteNodeId: raw.remoteNodeId,
        };

        try {
          const head = await callback(payload);
          await invoke(`${PLUGIN}|respond_to_request`, {
            args: {
              endpointHandle: Number(endpointHandle),
              reqHandle: bigintToSafeNumber(payload.reqHandle, "reqHandle"),
              status: head.status,
              headers: head.headers,
            },
          });
        } catch (err) {
          console.error("[iroh-http-tauri] handler error:", err);
          await invoke(`${PLUGIN}|respond_to_request`, {
            args: {
              endpointHandle: Number(endpointHandle),
              reqHandle: bigintToSafeNumber(BigInt(raw.reqHandle), "reqHandle"),
              status: 500,
              headers: [],
            },
          }).catch(() => {});
        }
      })();

      pending.add(task);
      task.finally(() => pending.delete(task));
    };

    const started = invoke<void>(`${PLUGIN}|serve`, {
      args: {
        endpointHandle: Number(endpointHandle),
        maxConcurrency: options.serveOptions?.maxConcurrency ?? null,
        maxConnectionsPerPeer: options.serveOptions?.maxConnectionsPerPeer ??
          null,
        requestTimeout: options.serveOptions?.requestTimeout ?? null,
        maxRequestBodyWireBytes:
          options.serveOptions?.maxRequestBodyWireBytes ?? null,
        maxRequestBodyDecodedBytes:
          options.serveOptions?.maxRequestBodyDecodedBytes ?? null,
        maxTotalConnections: options.serveOptions?.maxTotalConnections ?? null,
        drainTimeout: options.serveOptions?.drainTimeout ?? null,
        loadShed: options.serveOptions?.loadShed ?? null,
        decompress: options.serveOptions?.decompress ?? null,
      },
      channel,
      connChannel,
    });

    // Only wait for the loop to stop AFTER `serve` has resolved. The `serve`
    // command installs this cycle's loop-done signal (via `set_serve_handle`)
    // before it returns, so awaiting it guarantees `wait_serve_stop` observes
    // THIS serve loop. Issuing the two IPC calls unordered (the old behaviour)
    // let `wait_serve_stop` run first and latch the PREVIOUS cycle's already-
    // `true` done signal — resolving `finished` against a stale loop and
    // leaving the shared `serveRunning` guard corrupted across serve/stop
    // cycles, so a later serve() threw "serve() is already running" (#336).
    return started.then(
      async () => {
        // `stopServe` may race the asynchronous `serve` command and arrive
        // before Rust has registered this cycle's native handle. Retry that
        // early stop now that registration is known to be complete.
        if (this.#serveStopRequested) {
          await invoke(`${PLUGIN}|stop_serve`, {
            endpointHandle: Number(endpointHandle),
          }).catch(() => {});
        }
        // Resolve only after the native serve loop has stopped AND all
        // in-flight JS handler tasks have settled (responses written), so the
        // serve/finished contract holds the same as Node and Deno.
        await invoke<void>(`${PLUGIN}|wait_serve_stop`, {
          endpointHandle: Number(endpointHandle),
        });
        await Promise.allSettled([...pending]);
      },
      (err: unknown) => {
        // `serve` itself failed to start: surface it and settle `finished`
        // rather than hanging on `wait_serve_stop` for a loop that never ran.
        console.error("[iroh-http-tauri] serve error:", err);
        return Promise.allSettled([...pending]).then(() => {
          throw err;
        });
      },
    ).finally(() => {
      this.#serveStopRequested = false;
    });
  }

  closeEndpoint(handle: number, force?: boolean): Promise<void> {
    return invoke(`${PLUGIN}|close_endpoint`, {
      endpointHandle: Number(handle),
      force: force ?? null,
    });
  }

  stopServe(handle: number): void {
    this.#serveStopRequested = true;
    void invoke(`${PLUGIN}|stop_serve`, { endpointHandle: Number(handle) })
      .catch(() => {});
  }

  waitEndpointClosed(handle: number): Promise<void> {
    return invoke<void>(`${PLUGIN}|wait_endpoint_closed`, {
      endpointHandle: Number(handle),
    }).then(() => {});
  }

  async nodeAddr(handle: number): Promise<NodeAddrInfo> {
    return invoke<NodeAddrInfo>(`${PLUGIN}|node_addr`, {
      endpointHandle: Number(handle),
    });
  }

  async nodeTicket(handle: number): Promise<string> {
    return invoke<string>(`${PLUGIN}|node_ticket`, {
      endpointHandle: Number(handle),
    });
  }

  async homeRelay(handle: number): Promise<string | null> {
    return invoke<string | null>(`${PLUGIN}|home_relay`, {
      endpointHandle: Number(handle),
    });
  }

  override async discoveryInfo(handle: number): Promise<DiscoveryInfo> {
    return invoke<DiscoveryInfo>(`${PLUGIN}|discovery_info`, {
      endpointHandle: Number(handle),
    });
  }

  async peerInfo(handle: number, nodeId: string): Promise<NodeAddrInfo | null> {
    return invoke<NodeAddrInfo | null>(`${PLUGIN}|peer_info`, {
      endpointHandle: Number(handle),
      nodeId,
    });
  }

  async peerStats(handle: number, nodeId: string): Promise<PeerStats | null> {
    return invoke<PeerStats | null>(`${PLUGIN}|peer_stats`, {
      endpointHandle: Number(handle),
      nodeId,
    });
  }

  async stats(handle: number): Promise<EndpointStats> {
    return invoke<EndpointStats>(`${PLUGIN}|endpoint_stats`, {
      endpointHandle: Number(handle),
    });
  }

  override get sessionFns(): RawSessionFns {
    return this.#sessionFnsCache;
  }

  async browsePeers(handle: number, serviceName: string): Promise<number> {
    return invoke<number>(`${PLUGIN}|browse_peers`, {
      endpointHandle: Number(handle),
      serviceName,
    });
  }

  async browsePeersNext(
    browseHandle: number,
  ): Promise<PeerDiscoveryEvent | null> {
    const ev = await invoke<
      { isActive: boolean; nodeId: string; addrs: string[] } | null
    >(`${PLUGIN}|browse_peers_next`, {
      browseHandle: Number(browseHandle),
    });
    if (!ev) return null;
    return {
      type: ev.isActive ? "discovered" : "expired",
      nodeId: ev.nodeId,
      addrs: ev.addrs,
    };
  }

  browsePeersClose(browseHandle: number): Promise<void> {
    return invoke<void>(`${PLUGIN}|browse_peers_close`, {
      browseHandle: Number(browseHandle),
    });
  }

  async advertisePeer(handle: number, serviceName: string): Promise<number> {
    return invoke<number>(`${PLUGIN}|advertise_peer`, {
      endpointHandle: Number(handle),
      serviceName,
    });
  }

  advertisePeerClose(advertiseHandle: number): Promise<void> {
    return invoke<void>(`${PLUGIN}|advertise_peer_close`, {
      advertiseHandle: Number(advertiseHandle),
    });
  }

  // ── Generic DNS-SD ─────────────────────────────────────────────

  async advertise(config: ServiceConfig): Promise<number> {
    return invoke<number>(`${PLUGIN}|advertise`, {
      config: {
        serviceName: config.serviceName,
        instanceName: config.instanceName,
        port: config.port,
        addrs: config.addrs ?? [],
        txt: config.txt ?? {},
        protocol: config.protocol,
      },
    });
  }

  advertiseClose(advertiseHandle: number): Promise<void> {
    return invoke<void>(`${PLUGIN}|advertise_close`, {
      advertiseHandle: Number(advertiseHandle),
    });
  }

  async browse(
    serviceName: string,
    protocol?: DnsSdProtocol,
  ): Promise<number> {
    return invoke<number>(`${PLUGIN}|browse`, {
      serviceName,
      protocol,
    });
  }

  async browseNext(
    browseHandle: number,
  ): Promise<ServiceRecord | null> {
    return invoke<ServiceRecord | null>(`${PLUGIN}|browse_next`, {
      browseHandle: Number(browseHandle),
    });
  }

  browseClose(browseHandle: number): Promise<void> {
    return invoke<void>(`${PLUGIN}|browse_close`, {
      browseHandle: Number(browseHandle),
    });
  }

  // ── Transport events ──────────────────────────────────────────────────────

  override startTransportEvents(
    endpointHandle: number,
    callback: (event: TransportEventPayload) => void,
  ): void {
    const channel = new Channel<TransportEventPayload>();
    channel.onmessage = callback;
    void invoke(`${PLUGIN}|start_transport_events`, {
      endpointHandle: Number(endpointHandle),
      channel,
    }).catch((err: unknown) =>
      console.error("[iroh-http-tauri] startTransportEvents error:", err)
    );
  }

  override async nextPathChange(
    endpointHandle: number,
    nodeId: string,
    subscriptionId: number,
  ): Promise<PathInfo | null> {
    return invoke<PathInfo | null>(`${PLUGIN}|next_path_change`, {
      endpointHandle: Number(endpointHandle),
      nodeId,
      subscriptionId,
    });
  }

  override async unsubscribePathChanges(
    endpointHandle: number,
    nodeId: string,
    subscriptionId: number,
  ): Promise<void> {
    await invoke<void>(`${PLUGIN}|unsubscribe_path_changes`, {
      endpointHandle: Number(endpointHandle),
      nodeId,
      subscriptionId,
    });
  }
}

function makeTauriSessionFns(epHandle: number): RawSessionFns {
  return {
    connect: async (endpointHandle, nodeId, directAddrs) => {
      return invoke<number>(`${PLUGIN}|session_connect`, {
        args: {
          endpointHandle: Number(endpointHandle),
          nodeId,
          directAddrs: directAddrs ?? null,
        },
      }).then(BigInt);
    },
    sessionAccept: async (endpointHandle) => {
      const res = await invoke<
        { sessionHandle: number; nodeId: string } | null
      >(
        `${PLUGIN}|session_accept`,
        { endpointHandle: Number(endpointHandle) },
      );
      if (res === null) return null;
      return { sessionHandle: BigInt(res.sessionHandle), nodeId: res.nodeId };
    },
    createBidiStream: async (sessionHandle) => {
      const res = await invoke<{ readHandle: number; writeHandle: number }>(
        `${PLUGIN}|session_create_bidi_stream`,
        { endpointHandle: epHandle, sessionHandle: Number(sessionHandle) },
      );
      return {
        readHandle: BigInt(res.readHandle),
        writeHandle: BigInt(res.writeHandle),
      } satisfies FfiDuplexStream;
    },
    nextBidiStream: async (sessionHandle) => {
      const res = await invoke<
        { readHandle: number; writeHandle: number } | null
      >(
        `${PLUGIN}|session_next_bidi_stream`,
        { endpointHandle: epHandle, sessionHandle: Number(sessionHandle) },
      );
      return res
        ? ({
          readHandle: BigInt(res.readHandle),
          writeHandle: BigInt(res.writeHandle),
        } satisfies FfiDuplexStream)
        : null;
    },
    createUniStream: async (sessionHandle) => {
      return invoke<number>(`${PLUGIN}|session_create_uni_stream`, {
        endpointHandle: epHandle,
        sessionHandle: Number(sessionHandle),
      }).then(BigInt);
    },
    nextUniStream: async (sessionHandle) => {
      const h = await invoke<number | null>(
        `${PLUGIN}|session_next_uni_stream`,
        {
          endpointHandle: epHandle,
          sessionHandle: Number(sessionHandle),
        },
      );
      return h != null ? BigInt(h) : null;
    },
    sendDatagram: async (sessionHandle, data) => {
      const b64 = encodeBase64(data);
      await invoke<void>(`${PLUGIN}|session_send_datagram`, {
        endpointHandle: epHandle,
        sessionHandle: Number(sessionHandle),
        data: b64,
      });
    },
    recvDatagram: async (sessionHandle) => {
      const res = await invoke<string | null>(
        `${PLUGIN}|session_recv_datagram`,
        {
          endpointHandle: epHandle,
          sessionHandle: Number(sessionHandle),
        },
      );
      if (res === null) return null;
      const bin = atob(res);
      const out = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
      return out;
    },
    maxDatagramSize: async (sessionHandle) => {
      return invoke<number | null>(`${PLUGIN}|session_max_datagram_size`, {
        endpointHandle: epHandle,
        sessionHandle: Number(sessionHandle),
      });
    },
    closed: async (sessionHandle) => {
      return invoke<{ closeCode: number; reason: string }>(
        `${PLUGIN}|session_closed`,
        {
          endpointHandle: epHandle,
          sessionHandle: Number(sessionHandle),
        },
      );
    },
    close: async (sessionHandle, closeCode?, reason?) => {
      await invoke<void>(`${PLUGIN}|session_close`, {
        endpointHandle: epHandle,
        sessionHandle: Number(sessionHandle),
        closeCode,
        reason,
      });
    },
  };
}

// ── Mobile lifecycle listener ─────────────────────────────────────────────────

function installLifecycleListener(
  endpointHandle: number,
  options: { auto?: boolean; maxRetries?: number },
  onDead: () => void,
): (() => void) | undefined {
  if (typeof document === "undefined") return;
  const isMobile = /android|iphone|ipad/i.test(navigator.userAgent);
  if (!reconnectEnabled(options.auto, isMobile)) return;

  // The probe checks real transport liveness (the native `ping` command now
  // returns whether the QUIC transport is usable, not merely that the handle
  // exists). A `false` result or a throw both mean the transport is gone and
  // recovery must run (#336).
  return installForegroundHealthCheck({
    probe: () => invoke<boolean>(`${PLUGIN}|ping`, { endpointHandle }),
    onUnhealthy: onDead,
    enabled: true,
    maxRetries: options.maxRetries ?? 3,
  });
}

/**
 * Whether the foreground reconnect listener should be installed.
 *
 * An explicit `auto: false` always disables it — including on mobile, where the
 * listener was previously force-enabled and could permanently close a node
 * whose reconnect the caller had deliberately disabled (#350 F3). Otherwise it
 * is enabled when the caller opts in (`auto: true`) or implicitly on mobile,
 * where a suspended socket must be recovered on foreground.
 */
export function reconnectEnabled(
  auto: boolean | undefined,
  isMobile: boolean,
): boolean {
  if (auto === false) return false;
  return auto === true || isMobile;
}

// ── Public API ────────────────────────────────────────────────────────────────

/**
 * Create an Iroh node — the entry point for peer-to-peer HTTP in a Tauri app.
 *
 * A node is both client and server: call {@link IrohNode.fetch} to send requests
 * to peers and {@link IrohNode.serve} to handle incoming ones. Each node has a
 * stable Ed25519 identity ({@link IrohNode.publicKey}) that peers use to address
 * it — there is no DNS. The node runs in the Rust process; this function drives
 * it over Tauri's `invoke` bridge.
 *
 * @param options Optional configuration ({@link NodeOptions}). Omit `key` to
 *   generate a fresh identity; pass a saved `key` to keep a stable node ID across
 *   restarts. Relay, discovery, and tuning are all configured here.
 * @returns A ready-to-use {@link IrohNode}.
 *
 * SECURITY: the endpoint's private key is kept **native-held** in the Rust
 * process and is never handed to the webview. When you pass your own `key`,
 * `node.secretKey` returns that key (you already hold it) and the type narrows
 * to {@link IrohNodeWithSecret}. When you omit `key`, the identity is generated
 * natively and there is *no* mechanism to export it — `node.secretKey` is
 * `undefined`. This matches Node.js and Deno exactly. To persist an identity,
 * generate a key in JS (`SecretKey.generate()`), pass it to `createNode({ key })`,
 * and store its bytes yourself.
 *
 * @example
 * ```ts
 * import { createNode, SecretKey } from "@momics/iroh-http-tauri";
 *
 * // Ephemeral identity — secretKey is undefined (native-held, never exported).
 * const node = await createNode();
 *
 * // Persistent identity — you own the key.
 * const key = SecretKey.generate();
 * const node2 = await createNode({ key });
 * node2.secretKey; // the key you passed in
 *
 * const server = node.serve((req) => new Response("hello"));
 * const res = await node.fetch(`httpi://${peerId}/`);
 * console.log(await res.text());
 * await node.close();
 * ```
 */
export function createNode(
  options: NodeOptions & { key: SecretKey | Uint8Array },
): Promise<IrohNodeWithSecret>;
export function createNode(options?: NodeOptions): Promise<IrohNode>;
export async function createNode(options?: NodeOptions): Promise<IrohNode> {
  const keyBytes: string | null = options?.key
    ? encodeBase64(
      options.key instanceof Uint8Array
        ? options.key
        : (options.key as SecretKey).toBytes(),
    )
    : null;

  const { relayMode, relays, disableNetworking } = normaliseRelayMode(
    options?.relay,
  );
  const discovery = normaliseDiscovery(options?.discovery);
  const compression = normaliseCompression(options?.compression);
  const bindAddrs = options?.bindAddr
    ? (Array.isArray(options.bindAddr) ? options.bindAddr : [options.bindAddr])
    : null;

  const info = await invoke<{
    endpointHandle: number;
    nodeId: string;
  }>(`${PLUGIN}|create_endpoint`, {
    args: options
      ? {
        key: keyBytes,
        idleTimeout: options.connections?.idleTimeoutMs ?? null,
        relayMode: relayMode ?? null,
        relays,
        bindAddrs,
        dnsDiscovery: discovery.dnsServerUrl ?? null,
        dnsDiscoveryEnabled: discovery.dnsEnabled,
        channelCapacity: options.internals?.channelCapacity ?? null,
        maxChunkSizeBytes: options.internals?.maxChunkSizeBytes ?? null,
        handleTtl: options.internals?.handleTtl ?? null,
        maxPooledConnections: options.connections?.maxPooled ?? null,
        poolIdleTimeoutMs: options.connections?.poolIdleTimeoutMs ?? null,
        disableNetworking,
        proxyUrl: options.proxy?.url ?? null,
        proxyFromEnv: options.proxy?.fromEnv ?? null,
        keylog: options.debug?.keylog ?? null,
        compressionLevel: compression.level ?? null,
        compressionMinBodyBytes: compression.minBodyBytes ?? null,
        maxHeaderBytes: options.limits?.maxHeaderBytes ?? null,
      }
      : null,
  }).catch((e: unknown) => {
    throw classifyBindError(e);
  });

  const epHandle = Number(info.endpointHandle);
  const adapter = new TauriAdapter(epHandle);
  const nativeClosed = invoke<void>(`${PLUGIN}|wait_endpoint_closed`, {
    endpointHandle: Number(info.endpointHandle),
  }).then(() => {});

  const node = IrohNode._create(
    adapter,
    {
      endpointHandle: Number(info.endpointHandle),
      nodeId: info.nodeId,
    },
    options,
    nativeClosed,
  );

  const reconnect = options?.reconnect;
  if (reconnect) {
    let unsubscribeLifecycle: (() => void) | undefined;
    const releaseLifecycleListener = (): void => {
      const release = unsubscribeLifecycle;
      unsubscribeLifecycle = undefined;
      release?.();
    };
    const reportReconnectError = (error: unknown): void => {
      const onError = reconnect.onReconnectError;
      if (!onError) {
        console.error("[iroh-http-tauri] foreground reconnect error:", error);
        return;
      }
      try {
        // Although the public callback is synchronous, assimilating its return
        // also prevents an accidentally-async reporter from creating another
        // unhandled rejection while it is surfacing the original failure.
        const reported = onError(error);
        void Promise.resolve(reported).catch((reportingError) => {
          console.error(
            "[iroh-http-tauri] onReconnectError rejected:",
            reportingError,
            "Original reconnect error:",
            error,
          );
        });
      } catch (reportingError) {
        console.error(
          "[iroh-http-tauri] onReconnectError threw:",
          reportingError,
          "Original reconnect error:",
          error,
        );
      }
    };
    const closeUnusableNode = async (): Promise<void> => {
      try {
        await node.close();
      } catch (closeError) {
        reportReconnectError(closeError);
      }
    };

    unsubscribeLifecycle = installLifecycleListener(
      Number(info.endpointHandle),
      reconnect,
      () => {
        // The old node owns this listener only until it hands recovery off.
        // Release it first so foreground events cannot request two replacement
        // nodes while the application's callback is still pending.
        releaseLifecycleListener();

        // #350 F3: a foreground probe found the transport dead. If the app
        // supplied a recovery callback, hand off so it can recreate the node
        // (e.g. `createNode({ key })` with the same identity) instead of the
        // node being silently, permanently closed. Without a callback, close
        // the node so its `closed` promise resolves and the app can react,
        // rather than leaving a half-dead handle in place.
        const recover = reconnect.onReconnectNeeded;
        if (recover) {
          // Promise assimilation handles both a synchronous throw and an async
          // rejection. If replacement fails, close the unusable old node so
          // `node.closed` gives the app a deterministic recovery signal. The
          // recovery failure and any subsequent close failure remain observable
          // through `onReconnectError` (or the console fallback).
          void Promise.resolve()
            .then(() => recover())
            .catch(async (recoveryError) => {
              reportReconnectError(recoveryError);
              await closeUnusableNode();
            });
        } else {
          void closeUnusableNode();
        }
      },
    );
    if (unsubscribeLifecycle) {
      const originalClose = node.close.bind(node);
      node.close = async (...args: Parameters<typeof originalClose>) => {
        releaseLifecycleListener();
        return originalClose(...args);
      };
    }
  }

  return node;
}

export type { IrohNode, NodeOptions };

/**
 * Test-only: construct a bare {@link TauriAdapter} for a fake endpoint handle so
 * the guest-js body-encoding path (`sendChunk`) can be exercised at the FFI
 * boundary without a real Rust backend. Not part of the public API.
 * @internal
 */
export function _createAdapterForTesting(epHandle: number): IrohAdapter {
  return new TauriAdapter(epHandle);
}

export {
  type ForegroundHealthCheckOptions,
  installForegroundHealthCheck,
  type LifecycleContext,
  type LifecycleHandle,
  type LifecycleOptions,
  type LifecycleResource,
  type LifecycleStartReason,
  type LifecycleState,
  type LifecycleStorage,
  withLifecycle,
} from "./lifecycle.js";
export {
  asIrohPeer,
  IROH_HTTP_SERVICE,
  isDialableSocketAddr,
  PublicKey,
  SecretKey,
  TXT_KEY_ADDRESS,
  TXT_KEY_PUBLIC_KEY,
  TXT_KEY_RELAY,
} from "@momics/iroh-http-shared";
export type {
  DnsSdAdvertiseOptions,
  DnsSdBrowseOptions,
  DnsSdProtocol,
  ServiceConfig,
  ServiceRecord,
} from "@momics/iroh-http-shared";
