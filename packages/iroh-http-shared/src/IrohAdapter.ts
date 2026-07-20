import type { EndpointStats, PathInfo, PeerStats } from "./observability.js";
import type {
  DnsSdProtocol,
  PeerDiscoveryEvent,
  ServiceConfig,
  ServiceRecord,
} from "./discovery.js";
import type { TransportEventPayload } from "./observability.js";
import type { RawSessionFns } from "./session.js";

export interface FfiRequest {
  method: string;
  url: string;
  headers: [string, string][];
  remoteNodeId: string;
}

export interface FfiResponseHead {
  status: number;
  headers: [string, string][];
}

export interface FfiResponse extends FfiResponseHead {
  bodyHandle: bigint;
  url: string;
  /** Base64-encoded response body returned inline to avoid extra FFI round-trips (#126). */
  inlineBody?: string | null;
}

export interface RequestPayload extends FfiRequest {
  reqHandle: bigint;
  reqBodyHandle: bigint;
  resBodyHandle: bigint;
}

export interface FfiDuplexStream {
  readHandle: bigint;
  writeHandle: bigint;
}

export interface BidirectionalStream {
  readable: ReadableStream<Uint8Array>;
  writable: WritableStream<Uint8Array>;
}

export interface PeerConnectionEvent {
  peerId: string;
  connected: boolean;
}

export interface EndpointInfo {
  endpointHandle: number;
  nodeId: string;
}

export interface NodeAddrInfo {
  id: string;
  addrs: string[];
}

/**
 * A node's discovery info: its id, its first dialable direct `ip:port` address
 * (or `null` when only loopback/link-local addresses are available), all of its
 * routable dialable direct addresses, and its home relay URL (or `null`).
 *
 * The `directAddress`/`directAddresses` carry the real bound QUIC port — unlike
 * the placeholder ports (`:0`/`:1`) that raw address enumeration can report — so
 * they can be advertised directly for LAN direct-dial instead of relay-only
 * fallback. `directAddresses` lists *every* routable candidate (a device may be
 * on several interfaces, e.g. a VPN `10.x` and the real LAN `192.168.x`), so an
 * advertiser can publish them all and let the dialing peer race the paths (#348).
 * `directAddress` is retained as the first candidate for back-compat.
 */
export interface DiscoveryInfo {
  nodeId: string;
  directAddress: string | null;
  directAddresses: string[];
  relayUrl: string | null;
}

export interface IrohFetchInit extends RequestInit {
  /**
   * Known direct QUIC addresses for the target peer in `"ip:port"` notation.
   * When provided, the node attempts these addresses before falling back to
   * relay-assisted hole-punching. Speeds up the first connection to a peer
   * whose address is already known out-of-band.
   */
  directAddrs?: string[];
  /**
   * Home relay URL for the target peer, usually obtained from discovery.
   * Supplying it lets a fresh node connect without first resolving the peer
   * through DNS. Iroh may still upgrade the connection to a direct path.
   */
  relayUrl?: string;
  /** Per-request timeout in milliseconds.  Overrides the endpoint-wide default. */
  requestTimeout?: number;
  /**
   * When `false`, the response body bytes are returned exactly as received
   * (no decompression layer).  Useful when forwarding a compressed payload
   * downstream or when the caller wants to decompress itself.
   * @default true
   */
  decompress?: boolean;
  /**
   * Per-call response body byte limit.  When set, overrides the endpoint-wide
   * `maxResponseBodyBytes` default for this single request.
   */
  maxResponseBodyBytes?: number;
}

export interface CloseOptions {
  force?: boolean;
}

/** Per-call fetch knobs threaded through rawFetch to the Rust stack. */
export interface FetchOptions {
  /** Per-request timeout in milliseconds.  Overrides endpoint default. */
  timeoutMs?: number;
  /** When `false`, the response body is not decompressed. @default true */
  decompress?: boolean;
  /** Per-call response body byte limit.  Overrides endpoint default. */
  maxResponseBodyBytes?: number;
}

export type RawFetchFn = (
  endpointHandle: number,
  nodeId: string,
  url: string,
  method: string,
  headers: [string, string][],
  reqBodyHandle: bigint | null,
  fetchToken: bigint,
  directAddrs: string[] | null,
  fetchOptions?: FetchOptions,
) => Promise<FfiResponse>;

export type AllocBodyWriterFn = () => bigint | Promise<bigint>;

export interface FfiServeOptions {
  maxConcurrency?: number;
  maxConnectionsPerPeer?: number;
  requestTimeout?: number;
  maxRequestBodyWireBytes?: number;
  maxRequestBodyDecodedBytes?: number;
  maxTotalConnections?: number;
  drainTimeout?: number;
  loadShed?: boolean;
  /** When `false`, request bodies are forwarded raw (no decompression). @default true */
  decompress?: boolean;
}

export type RawServeFn = (
  endpointHandle: number,
  options: {
    onConnectionEvent?: (event: PeerConnectionEvent) => void;
    serveOptions?: FfiServeOptions;
  },
  callback: (payload: RequestPayload) => Promise<FfiResponseHead>,
) => Promise<void>;

export abstract class IrohAdapter {
  // ── Required: body streaming ────────────────────────────────────────────────
  abstract nextChunk(handle: bigint): Promise<Uint8Array | null>;
  abstract sendChunk(handle: bigint, chunk: Uint8Array): Promise<void>;
  abstract finishBody(handle: bigint): Promise<void>;
  abstract cancelRequest(handle: bigint): Promise<void>;
  abstract allocFetchToken(endpointHandle: number): Promise<bigint>;
  abstract cancelFetch(token: bigint): void;
  abstract allocBodyWriter(endpointHandle: number): bigint | Promise<bigint>;

  // ── Required: raw transport ─────────────────────────────────────────────────
  abstract rawFetch(
    endpointHandle: number,
    nodeId: string,
    url: string,
    method: string,
    headers: [string, string][],
    reqBodyHandle: bigint | null,
    fetchToken: bigint,
    directAddrs: string[] | null,
    fetchOptions?: FetchOptions,
  ): Promise<FfiResponse>;

  abstract rawServe(
    endpointHandle: number,
    options: {
      onConnectionEvent?: (event: PeerConnectionEvent) => void;
      serveOptions?: FfiServeOptions;
    },
    callback: (payload: RequestPayload) => Promise<FfiResponseHead>,
  ): Promise<void>;

  // ── Required: endpoint lifecycle ────────────────────────────────────────────
  abstract closeEndpoint(handle: number, force?: boolean): Promise<void>;
  abstract stopServe(handle: number): void;
  abstract waitEndpointClosed(handle: number): Promise<void>;

  // ── Required: address / stats ───────────────────────────────────────────────
  abstract nodeAddr(endpointHandle: number): Promise<NodeAddrInfo>;
  abstract nodeTicket(endpointHandle: number): Promise<string>;
  abstract homeRelay(endpointHandle: number): Promise<string | null>;

  /**
   * Discovery info for this node: id, dialable direct address, and relay URL.
   *
   * The base implementation rejects; concrete adapters (node, deno, tauri)
   * override it with a real FFI call. Kept non-abstract so adapters that predate
   * this method — or lightweight test doubles — still type-check.
   */
  discoveryInfo(_endpointHandle: number): Promise<DiscoveryInfo> {
    return Promise.reject(
      new Error("discoveryInfo is not supported by this adapter"),
    );
  }
  abstract peerInfo(
    endpointHandle: number,
    nodeId: string,
  ): Promise<NodeAddrInfo | null>;
  abstract peerStats(
    endpointHandle: number,
    nodeId: string,
  ): Promise<PeerStats | null>;
  abstract stats(endpointHandle: number): Promise<EndpointStats>;

  // ── Optional: sessions ──────────────────────────────────────────────────────
  get sessionFns(): RawSessionFns | null {
    return null;
  }

  // ── Optional: mDNS discovery ────────────────────────────────────────────────
  browsePeers(_endpointHandle: number, _serviceName: string): Promise<number> {
    return Promise.reject(
      new Error(`browsePeers() not supported by this adapter`),
    );
  }
  browsePeersNext(_browseHandle: number): Promise<PeerDiscoveryEvent | null> {
    return Promise.reject(
      new Error(`browsePeersNext() not supported by this adapter`),
    );
  }
  browsePeersClose(_browseHandle: number): void | Promise<void> {/* no-op */}
  advertisePeer(
    _endpointHandle: number,
    _serviceName: string,
  ): Promise<number> {
    return Promise.reject(
      new Error(`advertisePeer() not supported by this adapter`),
    );
  }
  advertisePeerClose(_advertiseHandle: number): void | Promise<void> {
    /* no-op */
  }

  // ── Optional: generic DNS-SD ────────────────────────────────────────────────
  advertise(_config: ServiceConfig): Promise<number> {
    return Promise.reject(
      new Error(`advertise() not supported by this adapter`),
    );
  }
  advertiseClose(_advertiseHandle: number): void | Promise<void> {/* no-op */}
  browse(
    _serviceName: string,
    _protocol?: DnsSdProtocol,
  ): Promise<number> {
    return Promise.reject(
      new Error(`browse() not supported by this adapter`),
    );
  }
  browseNext(_browseHandle: number): Promise<ServiceRecord | null> {
    return Promise.reject(
      new Error(`browseNext() not supported by this adapter`),
    );
  }
  browseClose(_browseHandle: number): void | Promise<void> {/* no-op */}

  // ── Optional: transport events ──────────────────────────────────────────────
  // Transport events are delivered via a Rust-driven push mechanism: the
  // platform adapter starts a drain task that reads from the endpoint's mpsc
  // channel and calls `callback` for each event.  No JS polling loop needed.
  //
  // Platform support:
  //   Node  — ThreadsafeFunction; drain task runs in the napi thread pool.
  //   Deno  — FFI dispatch queue; JS adapter loops on nextTransportEvent.
  //   Tauri — Channel<T>; Tauri pipes events to the frontend Channel.
  //   Base  — no-op; override in each platform adapter.
  startTransportEvents(
    _endpointHandle: number,
    _callback: (event: TransportEventPayload) => void,
  ): void {/* no-op */}

  /**
   * Wait for the transport-event polling loop to finish.
   * Called by `IrohNode.close()` after `waitEndpointClosed` to ensure all
   * async FFI ops are settled before the close promise resolves.
   * Override in adapters that run a background polling loop (Deno).
   */
  drainTransportEvents(): Promise<void> {
    return Promise.resolve();
  }

  /** Subscribe to path changes for a specific peer. Blocks until a change occurs or the subscription ends. */
  nextPathChange(
    _endpointHandle: number,
    _nodeId: string,
    _subscriptionId: number,
  ): Promise<PathInfo | null> {
    return Promise.reject(
      new Error("nextPathChange() not supported by this adapter"),
    );
  }

  /** Stop an active path-change subscription for a specific peer. */
  unsubscribePathChanges(
    _endpointHandle: number,
    _nodeId: string,
    _subscriptionId: number,
  ): Promise<void> {
    return Promise.resolve();
  }
}
