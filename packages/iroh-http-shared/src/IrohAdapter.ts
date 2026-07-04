import type { EndpointStats, PathInfo, PeerStats } from "./observability.js";
import type { PeerDiscoveryEvent } from "./discovery.js";
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
  keypair?: Uint8Array;
}

export interface NodeAddrInfo {
  id: string;
  addrs: string[];
}

export interface IrohFetchInit extends RequestInit {
  /**
   * Known direct QUIC addresses for the target peer in `"ip:port"` notation.
   * When provided, the node attempts these addresses before falling back to
   * relay-assisted hole-punching. Speeds up the first connection to a peer
   * whose address is already known out-of-band.
   */
  directAddrs?: string[];
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
  maxServeErrors?: number;
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
  mdnsBrowse(_endpointHandle: number, _serviceName: string): Promise<number> {
    return Promise.reject(
      new Error(`mdnsBrowse() not supported by this adapter`),
    );
  }
  mdnsNextEvent(_browseHandle: number): Promise<PeerDiscoveryEvent | null> {
    return Promise.reject(
      new Error(`mdnsNextEvent() not supported by this adapter`),
    );
  }
  mdnsBrowseClose(_browseHandle: number): void {/* no-op */}
  mdnsAdvertise(
    _endpointHandle: number,
    _serviceName: string,
  ): Promise<number> {
    return Promise.reject(
      new Error(`mdnsAdvertise() not supported by this adapter`),
    );
  }
  mdnsAdvertiseClose(_advertiseHandle: number): void {/* no-op */}

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
  ): Promise<PathInfo | null> {
    return Promise.reject(
      new Error("nextPathChange() not supported by this adapter"),
    );
  }

  /** Stop an active path-change subscription for a specific peer. */
  unsubscribePathChanges(
    _endpointHandle: number,
    _nodeId: string,
  ): Promise<void> {
    return Promise.resolve();
  }
}
