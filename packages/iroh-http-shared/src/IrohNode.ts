import { PublicKey, resolveNodeId } from "./PublicKey.js";
import { SecretKey } from "./SecretKey.js";
import { type FetchFn, makeFetch } from "./fetch.js";
import {
  makeServe,
  type ServeFn,
  type ServeHandle,
  type ServeHandler,
  type ServeOptions,
} from "./serve.js";
import {
  buildSession,
  type IrohSession,
  type WebTransportCloseInfo,
} from "./session.js";
import {
  type CloseOptions,
  type EndpointInfo,
  type IrohAdapter,
  type IrohFetchInit,
  type NodeAddrInfo,
  type PeerConnectionEvent,
} from "./IrohAdapter.js";
import type {
  DiagnosticsEventDetail,
  EndpointStats,
  PathChangeEventDetail,
  PathInfo,
  PeerConnectEventDetail,
  PeerDisconnectEventDetail,
  PeerStats,
  TransportEventPayload,
} from "./observability.js";
import type {
  AdvertiseOptions,
  BrowseOptions,
  DiscoveredPeer,
  PeerDiscoveryEvent,
} from "./discovery.js";
import type { NodeOptions } from "./options/NodeOptions.js";

const _INTERNAL = Symbol("IrohNode._create");

export class IrohNode extends EventTarget {
  readonly publicKey: PublicKey;
  readonly secretKey: SecretKey;
  readonly closed: Promise<WebTransportCloseInfo>;

  #adapter: IrohAdapter;
  #endpointHandle: number;
  #nodeId: string;
  #nativeClosed: Promise<void>;
  #resolveClose!: (info: WebTransportCloseInfo) => void;
  #fetchFn: FetchFn;
  #serveFn: ServeFn;

  private constructor(
    guard: symbol,
    adapter: IrohAdapter,
    info: EndpointInfo,
    options: NodeOptions | undefined,
    nativeClosed: Promise<void>,
  ) {
    if (guard !== _INTERNAL) {
      throw new TypeError("IrohNode must be created via IrohNode._create()");
    }
    super();
    this.#adapter = adapter;
    this.#endpointHandle = info.endpointHandle;
    this.#nodeId = info.nodeId;
    this.#nativeClosed = nativeClosed;

    let resolveClose!: (info: WebTransportCloseInfo) => void;
    this.closed = new Promise<WebTransportCloseInfo>((r) => {
      resolveClose = r;
    });
    this.#resolveClose = resolveClose;

    nativeClosed.then(() =>
      resolveClose({ closeCode: 0, reason: "native shutdown" })
    );

    this.publicKey = PublicKey.fromString(info.nodeId);
    this.secretKey = SecretKey._fromBytesWithPublicKey(
      info.keypair,
      this.publicKey,
    );

    this.#fetchFn = makeFetch(adapter, info.endpointHandle);
    this.#serveFn = makeServe(
      adapter,
      info.endpointHandle,
      this.closed.then(() => {}),
      (ev: PeerConnectionEvent) => {
        if (ev.connected) {
          this.dispatchEvent(
            new CustomEvent<PeerConnectEventDetail>("peerconnect", {
              detail: { nodeId: ev.peerId },
            }),
          );
        } else {
          this.dispatchEvent(
            new CustomEvent<PeerDisconnectEventDetail>("peerdisconnect", {
              detail: { nodeId: ev.peerId },
            }),
          );
        }
      },
    );

    // Always start the transport event loop. It delivers "pathchange" and
    // "diagnostics" CustomEvents on this node. No opt-in required.
    this.#startTransportEvents();
  }

  static _create(
    adapter: IrohAdapter,
    info: EndpointInfo,
    options: NodeOptions | undefined,
    nativeClosed: Promise<void>,
  ): IrohNode {
    return new IrohNode(_INTERNAL, adapter, info, options, nativeClosed);
  }

  fetch(input: string | URL, init?: IrohFetchInit): Promise<Response> {
    return this.#fetchFn(input, init);
  }

  serve(handler: ServeHandler): ServeHandle;
  serve(options: ServeOptions, handler: ServeHandler): ServeHandle;
  serve(...args: unknown[]): ServeHandle {
    return (this.#serveFn as (...a: unknown[]) => ServeHandle)(...args);
  }

  async dial(
    peer: PublicKey | string,
    init?: { directAddrs?: string[] },
  ): Promise<IrohSession> {
    const sessionFns = this.#adapter.sessionFns;
    if (!sessionFns) {
      throw new Error("dial() not supported by this platform adapter");
    }
    const nodeId = resolveNodeId(peer);
    const directAddrs = init?.directAddrs ?? null;
    const sessionHandle = await sessionFns.connect(
      this.#endpointHandle,
      nodeId,
      directAddrs,
    );
    const remotePk = PublicKey.fromString(nodeId);
    return buildSession(this.#adapter, sessionHandle, remotePk, sessionFns);
  }

  /**
   * Accept incoming raw QUIC sessions from remote peers.
   *
   * Returns an async iterable that yields one `IrohSession` for each peer
   * that calls `node.dial()`.  The iterable ends when the node closes or
   * the `signal` is aborted.
   *
   * ```ts
   * for await (const session of node.incoming()) {
   *   console.log("peer connected:", session.remoteId.toString());
   * }
   *
   * // With shutdown signal:
   * const ac = new AbortController();
   * for await (const session of node.incoming({ signal: ac.signal })) { ... }
   * ac.abort();
   * ```
   */
  incoming(options?: { signal?: AbortSignal }): AsyncIterable<IrohSession> {
    const sessionFns = this.#adapter.sessionFns;
    if (!sessionFns?.sessionAccept) {
      throw new Error("incoming() not supported by this platform adapter");
    }
    const accept = sessionFns.sessionAccept.bind(sessionFns);
    const endpointHandle = this.#endpointHandle;
    const adapter = this.#adapter;
    const signal = options?.signal;

    return {
      [Symbol.asyncIterator]() {
        return {
          async next(): Promise<IteratorResult<IrohSession>> {
            if (signal?.aborted) {
              return { done: true, value: undefined };
            }
            const result = await accept(endpointHandle);
            if (result === null) {
              return { done: true, value: undefined };
            }
            const remotePk = PublicKey.fromString(result.nodeId);
            const session = buildSession(
              adapter,
              result.sessionHandle,
              remotePk,
              sessionFns,
            );
            return { done: false, value: session };
          },
          return(): Promise<IteratorResult<IrohSession>> {
            return Promise.resolve({ done: true, value: undefined });
          },
        };
      },
    };
  }

  /**
   * Discover peers on the local network via mDNS.
   *
   * Returns an async iterable that yields a {@link DiscoveredPeer} for each
   * mDNS announcement received. Mirroring iroh's discovery semantics, events
   * are **not** de-duplicated: an active peer re-announces periodically, so the
   * same `nodeId` is yielded repeatedly with `isActive: true`. A peer that ages
   * out is yielded once with `isActive: false`. The iterable ends when the node
   * closes or `options.signal` is aborted.
   *
   * @param options.serviceName mDNS service name to browse. Defaults to `"iroh-http"`.
   * @param options.signal Abort signal to stop browsing and end the iterable.
   *
   * ```ts
   * const ac = new AbortController();
   * for await (const peer of node.browse({ serviceName: "my-app", signal: ac.signal })) {
   *   console.log(peer.isActive ? "discovered" : "expired", peer.nodeId, peer.addrs);
   * }
   * ac.abort(); // stop browsing
   * ```
   */
  browse(options?: BrowseOptions): AsyncIterable<DiscoveredPeer> {
    const adapter = this.#adapter;
    const handle = this.#endpointHandle;
    const svcName = options?.serviceName ?? "iroh-http";
    const signal = options?.signal;

    return {
      [Symbol.asyncIterator]() {
        let browseHandle: number | null = null;
        return {
          async next(): Promise<IteratorResult<DiscoveredPeer>> {
            if (browseHandle === null) {
              browseHandle = await adapter.mdnsBrowse(handle, svcName);
            }
            if (signal?.aborted) {
              adapter.mdnsBrowseClose(browseHandle);
              browseHandle = null;
              return { done: true as const, value: undefined };
            }

            let event: PeerDiscoveryEvent | null;
            if (signal) {
              const abortPromise = new Promise<null>((resolve) => {
                if (signal.aborted) {
                  resolve(null);
                  return;
                }
                signal.addEventListener("abort", () => resolve(null), {
                  once: true,
                });
              });
              event = await Promise.race([
                adapter.mdnsNextEvent(browseHandle),
                abortPromise,
              ]);
              if (signal.aborted && browseHandle !== null) {
                adapter.mdnsBrowseClose(browseHandle);
                browseHandle = null;
                return { done: true as const, value: undefined };
              }
            } else {
              event = await adapter.mdnsNextEvent(browseHandle);
            }

            if (event === null) {
              return { done: true as const, value: undefined };
            }
            const discovered: DiscoveredPeer = {
              nodeId: event.nodeId,
              addrs: event.addrs ?? [],
              isActive: event.type === "discovered",
            };
            return { done: false as const, value: discovered };
          },
          return(): Promise<IteratorResult<DiscoveredPeer>> {
            if (browseHandle !== null) {
              adapter.mdnsBrowseClose(browseHandle);
              browseHandle = null;
            }
            return Promise.resolve({ done: true as const, value: undefined });
          },
        };
      },
    };
  }

  /**
   * Announce this node on the local network via mDNS so peers can discover it.
   *
   * Pass `options.signal` to control how long the node stays advertised: the
   * returned promise stays pending until the signal is aborted, then advertising
   * stops. Without a signal, the promise resolves immediately and the node
   * remains advertised for the lifetime of the node.
   *
   * @param options.serviceName mDNS service name to advertise under. Defaults to `"iroh-http"`.
   * @param options.signal Abort signal to stop advertising.
   *
   * ```ts
   * const ac = new AbortController();
   * // Resolves when `ac.abort()` is called (e.g. from a Ctrl+C handler).
   * const advertising = node.advertise({ serviceName: "my-app", signal: ac.signal });
   * // ... later, to stop advertising:
   * ac.abort();
   * await advertising;
   * ```
   */
  async advertise(options?: AdvertiseOptions): Promise<void> {
    const svcName = options?.serviceName ?? "iroh-http";
    const signal = options?.signal;
    const advHandle = await this.#adapter.mdnsAdvertise(
      this.#endpointHandle,
      svcName,
    );
    if (signal) {
      return new Promise<void>((resolve) => {
        signal.addEventListener("abort", () => {
          this.#adapter.mdnsAdvertiseClose(advHandle);
          resolve();
        }, { once: true });
        if (signal.aborted) {
          this.#adapter.mdnsAdvertiseClose(advHandle);
          resolve();
        }
      });
    }
  }

  async addr(): Promise<NodeAddrInfo> {
    return this.#adapter.nodeAddr(this.#endpointHandle);
  }

  async ticket(): Promise<string> {
    return this.#adapter.nodeTicket(this.#endpointHandle);
  }

  async homeRelay(): Promise<string | null> {
    return this.#adapter.homeRelay(this.#endpointHandle);
  }

  async peerInfo(peer: PublicKey | string): Promise<NodeAddrInfo | null> {
    return this.#adapter.peerInfo(this.#endpointHandle, resolveNodeId(peer));
  }

  async peerStats(peer: PublicKey | string): Promise<PeerStats | null> {
    return this.#adapter.peerStats(this.#endpointHandle, resolveNodeId(peer));
  }

  async stats(): Promise<EndpointStats> {
    return this.#adapter.stats(this.#endpointHandle);
  }

  pathChanges(
    peer: PublicKey | string,
    options?: { signal?: AbortSignal },
  ): AsyncIterable<PathInfo> {
    const nodeId = resolveNodeId(peer);
    const adapter = this.#adapter;
    const endpointHandle = this.#endpointHandle;
    const signal = options?.signal;

    return {
      [Symbol.asyncIterator]() {
        return {
          async next(): Promise<IteratorResult<PathInfo>> {
            if (signal?.aborted) {
              return { done: true, value: undefined };
            }
            const path = await adapter.nextPathChange(endpointHandle, nodeId);
            if (path === null) {
              return { done: true, value: undefined };
            }
            return { done: false, value: path };
          },
          return(): Promise<IteratorResult<PathInfo>> {
            // break / return from for-await — nothing to clean up on JS side;
            // Rust watcher exits when the channel sender is dropped.
            return Promise.resolve({ done: true, value: undefined });
          },
        };
      },
    };
  }

  #startTransportEvents(): void {
    this.#adapter.startTransportEvents(
      this.#endpointHandle,
      (event: TransportEventPayload) => {
        if (event.type === "path:change") {
          this.dispatchEvent(
            new CustomEvent<PathChangeEventDetail>("pathchange", {
              detail: {
                nodeId: event.peerId,
                relay: event.relay,
                addr: event.addr,
                timestamp: event.timestamp,
              },
            }),
          );
        } else {
          const { type, ...rest } = event;
          this.dispatchEvent(
            new CustomEvent<DiagnosticsEventDetail>("diagnostics", {
              detail: { kind: type, ...rest } as DiagnosticsEventDetail,
            }),
          );
        }
      },
    );
  }

  async close(options?: CloseOptions): Promise<void> {
    await this.#adapter.closeEndpoint(this.#endpointHandle, options?.force);
    this.#resolveClose({ closeCode: 0, reason: "" });
    await this.#nativeClosed;
    await this.#adapter.drainTransportEvents();
  }

  [Symbol.asyncDispose](): Promise<void> {
    return this.close();
  }
}
