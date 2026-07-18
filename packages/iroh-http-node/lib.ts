/**
 * iroh-http-node — Node.js entry point.
 */

import {
  advertise as napiAdvertise,
  advertiseClose as napiAdvertiseClose,
  advertisePeer as napiAdvertisePeer,
  advertisePeerClose as napiAdvertisePeerClose,
  browse as napiBrowse,
  browseClose as napiBrowseClose,
  browseNext as napiBrowseNext,
  browsePeers as napiBrowsePeers,
  browsePeersClose as napiBrowsePeersClose,
  browsePeersNext as napiBrowsePeersNext,
  closeEndpoint,
  createEndpoint,
  discoveryInfo as napiDiscoveryInfo,
  endpointStats as napiEndpointStats,
  homeRelay as napiHomeRelay,
  jsAllocBodyWriter,
  jsAllocFetchToken,
  jsCancelInFlight,
  jsCancelRequest,
  jsFinishBody,
  jsNextChunk,
  jsSendChunk,
  jsTryNextChunk,
  nextPathChange as napiNextPathChange,
  nodeAddr as napiNodeAddr,
  nodeTicket as napiNodeTicket,
  peerInfo as napiPeerInfo,
  peerStats as napiPeerStats,
  rawFetch as napiRawFetch,
  rawRespond as napiRawRespond,
  rawServe as napiRawServe,
  sessionAccept as napiSessionAccept,
  sessionClosed as napiSessionClosed,
  sessionCloseHandle as napiSessionClose,
  sessionConnect as napiSessionConnect,
  sessionCreateBidiStream as napiSessionCreateBidiStream,
  sessionCreateUniStream as napiSessionCreateUniStream,
  sessionMaxDatagramSize as napiSessionMaxDatagramSize,
  sessionNextBidiStream as napiSessionNextBidiStream,
  sessionNextUniStream as napiSessionNextUniStream,
  sessionRecvDatagram as napiSessionRecvDatagram,
  sessionSendDatagram as napiSessionSendDatagram,
  startTransportEvents as napiStartTransportEvents,
  stopServe as napiStopServe,
  unsubscribePathChanges as napiUnsubscribePathChanges,
  waitEndpointClosed as napiWaitEndpointClosed,
  waitServeStop as napiWaitServeStop,
} from "./index.js";

import {
  classifyBindError,
  type DiscoveryInfo,
  IrohNode,
  type IrohNodeWithSecret,
  type NodeAddrInfo,
  type NodeOptions,
  normaliseCompression,
  normaliseDiscovery,
  normaliseRelayMode,
  type SecretKey,
} from "@momics/iroh-http-shared";
import {
  type FetchOptions,
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
  PathInfo,
  PeerDiscoveryEvent,
  PeerStats,
} from "@momics/iroh-http-shared";
import type {
  DnsSdProtocol,
  ServiceConfig,
  ServiceRecord,
} from "@momics/iroh-http-shared";

// ── Graceful shutdown tracking ─────────────────────────────────────────────────
const _liveEndpoints = new Set<number>();

function _closeAllEndpoints(signal: string): void {
  const handles = [..._liveEndpoints];
  _liveEndpoints.clear();
  if (handles.length === 0) return;
  const deadline = setTimeout(
    () => process.exit(128 + (signal === "SIGTERM" ? 15 : 2)),
    2000,
  );
  if (deadline.unref) deadline.unref();
  Promise.all(handles.map((h) => closeEndpoint(h, true).catch(() => {}))).then(
    () => {
      process.exit(0);
    },
  );
}

process.once("SIGTERM", () => _closeAllEndpoints("SIGTERM"));
process.once("SIGINT", () => _closeAllEndpoints("SIGINT"));

// ── NodeAdapter ────────────────────────────────────────────────────────────────

class NodeAdapter extends IrohAdapter {
  #eh: number;
  #sessionFns: RawSessionFns;
  #serveStopRequested = false;

  constructor(endpointHandle: number) {
    super();
    this.#eh = endpointHandle;
    const eh = endpointHandle;
    this.#sessionFns = {
      connect: async (epHandle, nodeId, directAddrs) =>
        napiSessionConnect(epHandle, nodeId, directAddrs ?? undefined),
      sessionAccept: async (epHandle) => {
        const res = await napiSessionAccept(epHandle);
        if (res === null) return null;
        return { sessionHandle: res.sessionHandle, nodeId: res.nodeId };
      },
      createBidiStream: async (sessionHandle) => {
        const ffi = await napiSessionCreateBidiStream(eh, sessionHandle);
        return {
          readHandle: ffi.readHandle,
          writeHandle: ffi.writeHandle,
        } satisfies FfiDuplexStream;
      },
      nextBidiStream: async (sessionHandle) => {
        const ffi = await napiSessionNextBidiStream(eh, sessionHandle);
        return ffi
          ? ({
            readHandle: ffi.readHandle,
            writeHandle: ffi.writeHandle,
          } satisfies FfiDuplexStream)
          : null;
      },
      createUniStream: async (sessionHandle: bigint) =>
        napiSessionCreateUniStream(eh, sessionHandle),
      nextUniStream: async (sessionHandle: bigint) => {
        const h = await napiSessionNextUniStream(eh, sessionHandle);
        return h ?? null;
      },
      sendDatagram: async (sessionHandle: bigint, data: Uint8Array) =>
        napiSessionSendDatagram(eh, sessionHandle, data),
      recvDatagram: async (sessionHandle: bigint) => {
        const buf = await napiSessionRecvDatagram(eh, sessionHandle);
        return buf ? new Uint8Array(buf) : null;
      },
      maxDatagramSize: async (sessionHandle: bigint) =>
        napiSessionMaxDatagramSize(eh, sessionHandle) ?? null,
      closed: async (sessionHandle: bigint) => {
        const info = await napiSessionClosed(eh, sessionHandle);
        return { closeCode: Number(info.closeCode), reason: info.reason };
      },
      close: async (
        sessionHandle: bigint,
        closeCode?: number,
        reason?: string,
      ) =>
        napiSessionClose(
          eh,
          sessionHandle,
          closeCode !== undefined ? BigInt(closeCode) : undefined,
          reason,
        ),
    };
  }

  // ── Body streaming ──────────────────────────────────────────────────────────
  nextChunk(handle: bigint): Promise<Uint8Array | null> {
    // Sync fast path: if data is already buffered, skip async overhead.
    try {
      const chunk = jsTryNextChunk(this.#eh, handle);
      return Promise.resolve(chunk ? new Uint8Array(chunk) : null);
    } catch {
      // Channel empty or lock contended — fall back to async.
      return jsNextChunk(this.#eh, handle);
    }
  }
  sendChunk(handle: bigint, chunk: Uint8Array): Promise<void> {
    return jsSendChunk(this.#eh, handle, chunk);
  }
  finishBody(handle: bigint): Promise<void> {
    jsFinishBody(this.#eh, handle);
    return Promise.resolve();
  }
  cancelRequest(handle: bigint): Promise<void> {
    jsCancelRequest(this.#eh, handle);
    return Promise.resolve();
  }
  allocFetchToken(_endpointHandle: number): Promise<bigint> {
    return Promise.resolve(jsAllocFetchToken(this.#eh));
  }
  cancelFetch(token: bigint): void {
    jsCancelInFlight(this.#eh, token);
  }
  allocBodyWriter(_endpointHandle: number): bigint {
    return jsAllocBodyWriter(this.#eh);
  }

  // ── Raw transport ───────────────────────────────────────────────────────────
  async rawFetch(
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
    const res = await (napiRawFetch as (
      endpointHandle: number,
      nodeId: string,
      url: string,
      method: string,
      headers: string[][],
      reqBodyHandle: bigint | null,
      fetchToken: bigint,
      directAddrs: string[] | null,
      timeoutMs?: number | null,
      decompress?: boolean | null,
      maxResponseBodyBytes?: number | null,
    ) => Promise<{
      status: number;
      headers: string[][];
      bodyHandle: bigint;
      url: string;
    }>)(
      endpointHandle,
      nodeId,
      url,
      method,
      headers as string[][],
      reqBodyHandle ?? null,
      fetchToken,
      directAddrs ?? null,
      fetchOptions?.timeoutMs ?? undefined,
      fetchOptions?.decompress ?? undefined,
      fetchOptions?.maxResponseBodyBytes ?? undefined,
    );
    return {
      status: res.status,
      headers: res.headers as [string, string][],
      bodyHandle: res.bodyHandle,
      url: res.url,
    } satisfies FfiResponse;
  }

  async rawServe(
    endpointHandle: number,
    options: {
      onConnectionEvent?: (event: PeerConnectionEvent) => void;
      serveOptions?: Record<string, unknown>;
    },
    callback: (payload: RequestPayload) => Promise<FfiResponseHead>,
  ): Promise<void> {
    // `napiRawServe` registers the native serve handle asynchronously. A
    // caller may close the JS ServeHandle before that registration completes,
    // in which case the first `napiStopServe` is necessarily a no-op. Remember
    // the request and retry it once registration is known to be complete.
    this.#serveStopRequested = false;
    // #119: Track in-flight handler tasks so waitServeStop drains them before resolving.
    const pending = new Set<Promise<void>>();
    try {
      await napiRawServe(
        endpointHandle,
        (options.serveOptions ?? null) as Parameters<typeof napiRawServe>[1],
        (
          payload: Parameters<typeof napiRawServe>[2] extends
            (arg: infer A) => void ? A
            : never,
        ) => {
          const typed = payload as unknown as RequestPayload;
          const task = callback(typed)
            .then((head) => {
              try {
                napiRawRespond(
                  endpointHandle,
                  typed.reqHandle,
                  head.status,
                  head.headers as string[][],
                );
              } catch (respondErr) {
                console.error(
                  "[iroh-http-node] rawRespond failed:",
                  respondErr,
                );
              }
            })
            .catch((err: unknown) => {
              console.error("[iroh-http-node] serve handler error:", err);
              try {
                napiRawRespond(endpointHandle, typed.reqHandle, 500, []);
              } catch (respondErr) {
                console.error(
                  "[iroh-http-node] rawRespond (fallback 500) failed:",
                  respondErr,
                );
              }
            });
          pending.add(task);
          task.finally(() => pending.delete(task));
        },
        options.onConnectionEvent ?? null,
      );
      if (this.#serveStopRequested) napiStopServe(endpointHandle);
      await napiWaitServeStop(endpointHandle);
      await Promise.allSettled([...pending]);
    } finally {
      this.#serveStopRequested = false;
    }
  }

  // ── Lifecycle ───────────────────────────────────────────────────────────────
  closeEndpoint(handle: number, force?: boolean): Promise<void> {
    _liveEndpoints.delete(handle);
    return closeEndpoint(handle, force ?? null);
  }
  stopServe(handle: number): void {
    this.#serveStopRequested = true;
    napiStopServe(handle);
  }
  waitEndpointClosed(handle: number): Promise<void> {
    return napiWaitEndpointClosed(handle);
  }

  // ── Address / stats ─────────────────────────────────────────────────────────
  async nodeAddr(handle: number): Promise<NodeAddrInfo> {
    const info = napiNodeAddr(handle);
    return { id: info.id, addrs: info.addrs } satisfies NodeAddrInfo;
  }
  async nodeTicket(handle: number): Promise<string> {
    return napiNodeTicket(handle);
  }
  async homeRelay(handle: number): Promise<string | null> {
    return napiHomeRelay(handle) ?? null;
  }
  override async discoveryInfo(handle: number): Promise<DiscoveryInfo> {
    const info = napiDiscoveryInfo(handle);
    return {
      nodeId: info.nodeId,
      directAddress: info.directAddress ?? null,
      directAddresses: info.directAddresses ?? [],
      relayUrl: info.relayUrl ?? null,
    } satisfies DiscoveryInfo;
  }
  async peerInfo(handle: number, nodeId: string): Promise<NodeAddrInfo | null> {
    const info = await napiPeerInfo(handle, nodeId);
    return info
      ? ({ id: info.id, addrs: info.addrs } satisfies NodeAddrInfo)
      : null;
  }
  async peerStats(handle: number, nodeId: string): Promise<PeerStats | null> {
    const stats = await napiPeerStats(handle, nodeId);
    if (!stats) return null;
    return {
      relay: stats.relay,
      relayUrl: stats.relayUrl ?? null,
      paths: stats.paths,
      rttMs: stats.rttMs ?? null,
      bytesSent: stats.bytesSent ?? null,
      bytesReceived: stats.bytesReceived ?? null,
      lostPackets: stats.lostPackets ?? null,
      sentPackets: stats.sentPackets ?? null,
      congestionWindow: stats.congestionWindow ?? null,
    };
  }
  async stats(handle: number): Promise<EndpointStats> {
    const s = napiEndpointStats(handle);
    return {
      activeReaders: Number(s.activeReaders),
      activeWriters: Number(s.activeWriters),
      activeSessions: Number(s.activeSessions),
      totalHandles: Number(s.totalHandles),
      poolSize: Number(s.poolSize),
      activeConnections: Number(s.activeConnections),
      activeRequests: Number(s.activeRequests),
      activePathSubscriptions: Number(s.activePathSubscriptions),
      activePathWatchers: Number(s.activePathWatchers),
    };
  }

  // ── Sessions ────────────────────────────────────────────────────────────────
  override get sessionFns(): RawSessionFns {
    return this.#sessionFns;
  }

  // ── mDNS discovery ──────────────────────────────────────────────────────────
  async browsePeers(handle: number, serviceName: string): Promise<number> {
    return napiBrowsePeers(handle, serviceName);
  }
  async browsePeersNext(
    browseHandle: number,
  ): Promise<PeerDiscoveryEvent | null> {
    const ev = await napiBrowsePeersNext(browseHandle);
    if (!ev) return null;
    return {
      type: ev.isActive ? "discovered" : "expired",
      nodeId: ev.nodeId,
      addrs: ev.addrs,
    };
  }
  browsePeersClose(browseHandle: number): void {
    napiBrowsePeersClose(browseHandle);
  }
  async advertisePeer(handle: number, serviceName: string): Promise<number> {
    return napiAdvertisePeer(handle, serviceName);
  }
  advertisePeerClose(advertiseHandle: number): void {
    napiAdvertisePeerClose(advertiseHandle);
  }

  // ── Generic DNS-SD ──────────────────────────────────────────────────────────
  async advertise(config: ServiceConfig): Promise<number> {
    return napiAdvertise({
      serviceName: config.serviceName,
      instanceName: config.instanceName,
      port: config.port,
      addrs: config.addrs ?? [],
      txt: config.txt ?? {},
      protocol: config.protocol,
    });
  }
  advertiseClose(advertiseHandle: number): void {
    napiAdvertiseClose(advertiseHandle);
  }
  async browse(
    serviceName: string,
    protocol?: DnsSdProtocol,
  ): Promise<number> {
    return napiBrowse(serviceName, protocol);
  }
  async browseNext(
    browseHandle: number,
  ): Promise<ServiceRecord | null> {
    const rec = await napiBrowseNext(browseHandle);
    if (!rec) return null;
    return {
      isActive: rec.isActive,
      serviceType: rec.serviceType,
      instanceName: rec.instanceName,
      host: rec.host ?? undefined,
      port: rec.port,
      addrs: rec.addrs,
      txt: rec.txt,
    };
  }
  browseClose(browseHandle: number): void {
    napiBrowseClose(browseHandle);
  }

  // ── Transport events ────────────────────────────────────────────────────────
  override startTransportEvents(
    endpointHandle: number,
    callback: (event: TransportEventPayload) => void,
  ): void {
    void napiStartTransportEvents(endpointHandle, (json: string) => {
      try {
        callback(JSON.parse(json) as TransportEventPayload);
      } catch (err) {
        console.error("[iroh-http-node] transport event callback error:", err);
      }
    });
  }

  override async nextPathChange(
    endpointHandle: number,
    nodeId: string,
  ): Promise<PathInfo | null> {
    const json = await napiNextPathChange(endpointHandle, nodeId);
    if (json === null || json === undefined) return null;
    return JSON.parse(json) as PathInfo;
  }

  override unsubscribePathChanges(
    endpointHandle: number,
    nodeId: string,
  ): Promise<void> {
    napiUnsubscribePathChanges(endpointHandle, nodeId);
    return Promise.resolve();
  }
}

// ── Public API ────────────────────────────────────────────────────────────────

export {
  asIrohPeer,
  IROH_HTTP_SERVICE,
  PublicKey,
  SecretKey,
  TXT_KEY_ADDRESS,
  TXT_KEY_PUBLIC_KEY,
  TXT_KEY_RELAY,
} from "@momics/iroh-http-shared";
export type {
  DnsSdAdvertiseOptions,
  DnsSdBrowseOptions,
} from "@momics/iroh-http-shared";
export type {
  DnsSdProtocol,
  IrohNode,
  IrohNodeWithSecret,
  NodeOptions,
  ServiceConfig,
  ServiceRecord,
};
export type { DiscoveryInfo };

/**
 * Create an Iroh node — the entry point for peer-to-peer HTTP.
 *
 * A node is both client and server: call {@link IrohNode.fetch} to send requests
 * to peers and {@link IrohNode.serve} to handle incoming ones. Each node has a
 * stable Ed25519 identity ({@link IrohNode.publicKey}) that peers use to address
 * it — there is no DNS.
 *
 * @param options Optional configuration ({@link NodeOptions}). Omit `key` to
 *   generate a fresh identity; pass a saved `key` to keep a stable node ID across
 *   restarts. Relay, discovery, and tuning are all configured here.
 * @returns A ready-to-use {@link IrohNode}. When you pass a `key`, the returned
 *   node's `secretKey` is non-optional ({@link IrohNodeWithSecret}); omit `key`
 *   and the natively generated identity is never surfaced, so `secretKey` is
 *   `undefined`. To persist an identity, generate a key and pass it back in.
 *
 * @example
 * ```ts
 * import { createNode } from "@momics/iroh-http-node";
 *
 * const node = await createNode();
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
export async function createNode(
  options?: NodeOptions,
): Promise<IrohNode> {
  const keyBytes = options?.key
    ? options.key instanceof Uint8Array
      ? options.key
      : (options.key as SecretKey).toBytes()
    : undefined;

  const { relayMode, relays, disableNetworking } = normaliseRelayMode(
    options?.relay,
  );
  const discovery = normaliseDiscovery(options?.discovery);
  const compression = normaliseCompression(options?.compression);
  const disableNetworkingFinal = disableNetworking ||
    (options?.disableNetworking ?? false);
  const bindAddrs = options?.bindAddr
    ? Array.isArray(options.bindAddr) ? options.bindAddr : [options.bindAddr]
    : undefined;

  const info = await createEndpoint(
    options
      ? {
        key: keyBytes,
        idleTimeout: options.connections?.idleTimeoutMs,
        relayMode,
        relays: relays ?? undefined,
        bindAddrs,
        dnsDiscovery: discovery.dnsServerUrl ?? undefined,
        dnsDiscoveryEnabled: discovery.dnsEnabled,
        channelCapacity: options.internals?.channelCapacity,
        maxChunkSizeBytes: options.internals?.maxChunkSizeBytes,
        handleTtl: options.internals?.handleTtl,
        maxPooledConnections: options.connections?.maxPooled,
        poolIdleTimeoutMs: options.connections?.poolIdleTimeoutMs,
        disableNetworking: disableNetworkingFinal,
        proxyUrl: options.proxy?.url,
        proxyFromEnv: options.proxy?.fromEnv,
        keylog: options.debug?.keylog,
        compressionLevel: compression.level,
        compressionMinBodyBytes: compression.minBodyBytes,
        maxHeaderBytes: options.limits?.maxHeaderBytes,
      }
      : undefined,
  ).catch((e: unknown) => {
    throw classifyBindError(e);
  });

  const eh = info.endpointHandle;
  _liveEndpoints.add(eh);

  const adapter = new NodeAdapter(eh);
  const nativeClosed = napiWaitEndpointClosed(info.endpointHandle);

  return IrohNode._create(
    adapter,
    {
      endpointHandle: info.endpointHandle,
      nodeId: info.nodeId,
    },
    options,
    nativeClosed,
  );
}
