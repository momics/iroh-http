/**
 * iroh-http-tauri — guest-js entry point.
 */

import { Channel, invoke } from "@tauri-apps/api/core";
import {
  classifyBindError,
  encodeBase64,
  IrohNode,
  type NodeOptions,
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
  EndpointStats,
  NodeAddrInfo,
  PeerDiscoveryEvent,
  PeerStats,
} from "@momics/iroh-http-shared";

const PLUGIN = "plugin:iroh-http";

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

/**
 * Convert an opaque u64 resource handle (slotmap key: 32-bit slot + 32-bit
 * generation) to a JS number for IPC, rejecting values outside the safe
 * integer range rather than silently rounding and addressing the wrong
 * resource. Mirrors the Deno adapter's `bigintToSafeNumber` guard (#252).
 */
function u64Handle(value: bigint, field = "handle"): number {
  if (value < 0n || value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new RangeError(
      `[iroh-http-tauri] ${field} value ${value} exceeds safe integer range and cannot be safely serialised`,
    );
  }
  return Number(value);
}

// ── TauriAdapter ──────────────────────────────────────────────────────────────

class TauriAdapter extends IrohAdapter {
  #epHandle: number;
  #sessionFnsCache: RawSessionFns;

  constructor(epHandle: number) {
    super();
    this.#epHandle = epHandle;
    this.#sessionFnsCache = makeTauriSessionFns(epHandle);
  }

  nextChunk(handle: bigint): Promise<Uint8Array | null> {
    // Sync fast path: if data is already buffered, skip async IPC overhead.
    // Protocol: first byte is 0x01 (data follows) or 0x00 (EOF).
    const safeHandle = u64Handle(handle);
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

  sendChunk(handle: bigint, chunk: Uint8Array): Promise<void> {
    return invoke(`${PLUGIN}|send_chunk`, chunk, {
      headers: {
        "iroh-ep": String(this.#epHandle),
        "iroh-handle": String(u64Handle(handle)),
      },
    });
  }

  finishBody(handle: bigint): Promise<void> {
    return invoke(`${PLUGIN}|finish_body`, {
      endpointHandle: this.#epHandle,
      handle: u64Handle(handle),
    });
  }

  cancelRequest(handle: bigint): Promise<void> {
    return invoke(`${PLUGIN}|cancel_request`, {
      endpointHandle: this.#epHandle,
      handle: u64Handle(handle),
    });
  }

  allocFetchToken(endpointHandle: number): Promise<bigint> {
    return invoke<number>(`${PLUGIN}|create_fetch_token`, { endpointHandle })
      .then(BigInt);
  }

  cancelFetch(token: bigint): void {
    void invoke(`${PLUGIN}|cancel_in_flight`, {
      endpointHandle: this.#epHandle,
      token: u64Handle(token, "token"),
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
          ? u64Handle(reqBodyHandle, "reqBodyHandle")
          : null,
        fetchToken: fetchToken != null
          ? u64Handle(fetchToken, "fetchToken")
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
              reqHandle: u64Handle(payload.reqHandle, "reqHandle"),
              status: head.status,
              headers: head.headers,
            },
          });
        } catch (err) {
          console.error("[iroh-http-tauri] handler error:", err);
          await invoke(`${PLUGIN}|respond_to_request`, {
            args: {
              endpointHandle: Number(endpointHandle),
              reqHandle: u64Handle(BigInt(raw.reqHandle), "reqHandle"),
              status: 500,
              headers: [],
            },
          }).catch(() => {});
        }
      })();

      pending.add(task);
      task.finally(() => pending.delete(task));
    };

    invoke(`${PLUGIN}|serve`, {
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
        maxServeErrors: options.serveOptions?.maxServeErrors ?? null,
        drainTimeout: options.serveOptions?.drainTimeout ?? null,
        loadShed: options.serveOptions?.loadShed ?? null,
        decompress: options.serveOptions?.decompress ?? null,
      },
      channel,
      connChannel,
    }).catch((err: unknown) =>
      console.error("[iroh-http-tauri] serve error:", err)
    );

    // Resolve only after the native serve loop has stopped AND all in-flight
    // JS handler tasks have settled (responses written), so the serve/finished
    // contract holds the same as Node and Deno.
    return invoke<void>(`${PLUGIN}|wait_serve_stop`, {
      endpointHandle: Number(endpointHandle),
    }).then(() => Promise.allSettled([...pending])).then(() => {});
  }

  closeEndpoint(handle: number, force?: boolean): Promise<void> {
    return invoke(`${PLUGIN}|close_endpoint`, {
      endpointHandle: Number(handle),
      force: force ?? null,
    });
  }

  stopServe(handle: number): void {
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

  async mdnsBrowse(handle: number, serviceName: string): Promise<number> {
    return invoke<number>(`${PLUGIN}|mdns_browse`, {
      endpointHandle: Number(handle),
      serviceName,
    });
  }

  async mdnsNextEvent(
    browseHandle: number,
  ): Promise<PeerDiscoveryEvent | null> {
    return invoke<PeerDiscoveryEvent | null>(`${PLUGIN}|mdns_next_event`, {
      browseHandle: Number(browseHandle),
    });
  }

  mdnsBrowseClose(browseHandle: number): void {
    void invoke(`${PLUGIN}|mdns_browse_close`, {
      browseHandle: Number(browseHandle),
    });
  }

  async mdnsAdvertise(handle: number, serviceName: string): Promise<number> {
    return invoke<number>(`${PLUGIN}|mdns_advertise`, {
      endpointHandle: Number(handle),
      serviceName,
    });
  }

  mdnsAdvertiseClose(advertiseHandle: number): void {
    void invoke(`${PLUGIN}|mdns_advertise_close`, {
      advertiseHandle: Number(advertiseHandle),
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
      const b64 = btoa(String.fromCharCode(...data));
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
  if (!isMobile && !options.auto) return;

  let retries = 0;
  const maxRetries = options.maxRetries ?? 3;
  const handler = async () => {
    if (document.visibilityState !== "visible") return;
    retries = 0;
    while (retries < maxRetries) {
      try {
        await invoke(`${PLUGIN}|ping`, { endpointHandle });
        return;
      } catch {
        retries++;
        if (retries < maxRetries) {
          await new Promise<void>((r) => setTimeout(r, 100 * 2 ** retries));
        }
      }
    }
    onDead();
  };
  document.addEventListener("visibilitychange", handler);
  return () => document.removeEventListener("visibilitychange", handler);
}

function normaliseDiscovery(disc?: NodeOptions["discovery"]): {
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
 * @example
 * ```ts
 * import { createNode } from "@momics/iroh-http-tauri";
 *
 * const node = await createNode();
 * const server = node.serve((req) => new Response("hello"));
 * const res = await node.fetch(`httpi://${peerId}/`);
 * console.log(await res.text());
 * await node.close();
 * ```
 */
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
        compressionLevel: typeof options.compression === "object"
          ? options.compression.level ?? null
          : options.compression
          ? 3
          : null,
        compressionMinBodyBytes: typeof options.compression === "object"
          ? options.compression.minBodyBytes ?? null
          : null,
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
    const unsubscribe = installLifecycleListener(
      Number(info.endpointHandle),
      reconnect,
      () => {
        node.close().catch(() => {});
      },
    );
    if (unsubscribe) {
      const originalClose = node.close.bind(node);
      (node as IrohNode & { close: () => Promise<void> }).close = async () => {
        unsubscribe();
        return originalClose();
      };
    }
  }

  return node;
}

/**
 * Export raw endpoint secret key bytes for identity persistence.
 *
 * SECURITY: This command requires the `iroh-http:crypto` permission and hands
 * raw private-key bytes to the webview. Store them only in encrypted storage,
 * overwrite temporary copies after use, and never log or expose them to
 * untrusted code.
 */
export async function exportSecretKey(
  endpointHandle: number,
): Promise<Uint8Array> {
  const bytes = await invoke<number[]>(`${PLUGIN}|export_secret_key`, {
    endpointHandle,
  });
  return new Uint8Array(bytes);
}

export type { IrohNode, NodeOptions };
export { PublicKey, SecretKey } from "@momics/iroh-http-shared";
