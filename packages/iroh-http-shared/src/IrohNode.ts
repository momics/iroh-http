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
  /**
   * The node's secret key — present only when a `key` was supplied to
   * `createNode`. If you did not provide a key, the identity is generated
   * natively and never surfaced to JS, so this is `undefined` on every adapter.
   * To keep a stable identity, generate a key yourself and pass it back in:
   * `createNode({ key })`.
   */
  readonly secretKey: SecretKey | undefined;
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
    // A node exposes a secret key only when the caller supplied one. A key you
    // did not bring is never surfaced — even when the runtime generated it
    // natively — so `secretKey` is present iff `key` was passed to createNode,
    // uniformly across all adapters.
    const providedKey = options?.key;
    this.secretKey = providedKey
      ? SecretKey._fromBytesWithPublicKey(
        providedKey instanceof Uint8Array ? providedKey : providedKey.toBytes(),
        this.publicKey,
      )
      : undefined;

    const maxChunkSizeBytes = options?.internals?.maxChunkSizeBytes;
    this.#fetchFn = makeFetch(adapter, info.endpointHandle, maxChunkSizeBytes);
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
      maxChunkSizeBytes,
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

  /**
   * Send an HTTP request to a remote peer over the Iroh overlay network.
   *
   * Works like the web-standard `fetch`, except the target peer is addressed by
   * its public key embedded in an `httpi://` URL hostname rather than by DNS.
   * The connection is established lazily (relay-assisted hole-punching) on the
   * first request to a given peer.
   *
   * @param input An `httpi://<peer-public-key>/path` URL, as a string or `URL`.
   * @param init Standard `RequestInit` plus Iroh extras (`directAddrs`,
   *   `requestTimeout`, `decompress`, `maxResponseBodyBytes`). Supports
   *   cancellation via `init.signal`.
   * @returns The peer's `Response`.
   *
   * ```ts
   * const res = await node.fetch(`httpi://${peerId}/api/data`, {
   *   method: "POST",
   *   body: JSON.stringify({ hello: "world" }),
   * });
   * console.log(res.status, await res.text());
   * ```
   */

  fetch(input: string | URL, init?: IrohFetchInit): Promise<Response> {
    return this.#fetchFn(input, init);
  }

  /**
   * Serve HTTP requests from remote peers on this node's public endpoint.
   *
   * Each incoming request is delivered to `handler` as a web-standard `Request`
   * augmented with a `Peer-Id` header carrying the authenticated caller's public
   * key. The returned {@link ServeHandle} exposes `close()`, which stops this
   * server (leaving the node alive), and `finished`, which resolves when the
   * serve loop stops — via `close()`, `node.close()`, an aborted
   * `options.signal`, or a fatal error.
   *
   * @remarks
   * **Security:** this opens a *public* endpoint — any peer that knows this
   * node's public key can connect. Iroh authenticates peer *identity* but not
   * *authorization*; check `req.headers.get("Peer-Id")` and reject untrusted
   * peers.
   *
   * @param options Serve tuning (error handler, shutdown signal, concurrency and
   *   body-size limits). May be omitted to use defaults.
   * @param handler Async function mapping a `Request` to a `Response`.
   * @returns A {@link ServeHandle} whose `close()` stops this server and whose
   *   `finished` promise settles on shutdown.
   *
   * ```ts
   * const server = node.serve((req) => {
   *   if (req.headers.get("Peer-Id") !== trustedPeerId) {
   *     return new Response("Forbidden", { status: 403 });
   *   }
   *   return Response.json({ ok: true });
   * });
   * // Later, stop just this server without closing the node:
   * await server.close();
   * ```
   */
  serve(handler: ServeHandler): ServeHandle;
  serve(options: ServeOptions, handler: ServeHandler): ServeHandle;
  serve(...args: unknown[]): ServeHandle {
    return (this.#serveFn as (...a: unknown[]) => ServeHandle)(...args);
  }

  /**
   * Open a raw QUIC session to a remote peer for bidirectional, message-style
   * communication — the lower-level counterpart to {@link IrohNode.fetch}.
   *
   * Use this when you need a persistent, full-duplex channel rather than HTTP
   * request/response. The remote peer accepts the session via
   * {@link IrohNode.incoming}.
   *
   * @param peer The target peer, as a {@link PublicKey} or its string form.
   * @param init Optional `directAddrs` (known `"ip:port"` addresses) that speed
   *   up the initial connection when the peer's address is known out-of-band.
   * @returns The established {@link IrohSession}.
   * @throws If the platform adapter does not support raw sessions.
   *
   * ```ts
   * const session = await node.dial(peerId);
   * console.log("connected to", session.remoteId.toString());
   * ```
   */
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

  /**
   * Return this node's current address: its public key plus the set of direct
   * socket addresses it is currently reachable on.
   *
   * The address set changes over time as the node learns new paths, so call
   * this when you need a fresh snapshot to share out-of-band.
   *
   * @returns The node's id and current direct addresses.
   */
  async addr(): Promise<NodeAddrInfo> {
    return this.#adapter.nodeAddr(this.#endpointHandle);
  }

  /**
   * Return a shareable connection *ticket* for this node — an opaque,
   * self-contained string bundling the node's public key and current addresses.
   *
   * Hand the ticket to another peer out-of-band so it can connect without
   * resolving the address separately.
   *
   * @returns The encoded node ticket.
   */
  async ticket(): Promise<string> {
    return this.#adapter.nodeTicket(this.#endpointHandle);
  }

  /**
   * Return the URL of this node's current home relay, or `null` if the node is
   * not currently using a relay (e.g. it has only direct connectivity).
   *
   * @returns The home relay URL, or `null`.
   */
  async homeRelay(): Promise<string | null> {
    return this.#adapter.homeRelay(this.#endpointHandle);
  }

  /**
   * Look up the last known address information for a remote peer.
   *
   * @param peer The peer to query, as a {@link PublicKey} or its string form.
   * @returns The peer's id and known addresses, or `null` if this node has never
   *   seen the peer.
   */
  async peerInfo(peer: PublicKey | string): Promise<NodeAddrInfo | null> {
    return this.#adapter.peerInfo(this.#endpointHandle, resolveNodeId(peer));
  }

  /**
   * Return live connection statistics for a remote peer — RTT, byte counters,
   * packet loss, congestion window, and the active network paths.
   *
   * @param peer The peer to query, as a {@link PublicKey} or its string form.
   * @returns The peer's {@link PeerStats}, or `null` if there is no active
   *   connection to it.
   */
  async peerStats(peer: PublicKey | string): Promise<PeerStats | null> {
    return this.#adapter.peerStats(this.#endpointHandle, resolveNodeId(peer));
  }

  /**
   * Return endpoint-wide runtime statistics for this node: active readers,
   * writers, sessions, connections, in-flight requests, and the internal handle
   * pool size. Useful for diagnostics and leak detection.
   *
   * @returns A snapshot of this node's {@link EndpointStats}.
   */
  async stats(): Promise<EndpointStats> {
    return this.#adapter.stats(this.#endpointHandle);
  }

  /**
   * Observe network-path changes for a connection to a remote peer.
   *
   * Returns an async iterable that yields a {@link PathInfo} each time the active
   * path to `peer` changes — for example a relay-to-direct upgrade or a
   * successful hole-punch. The iterable ends when the node closes or
   * `options.signal` is aborted.
   *
   * @param peer The peer to watch, as a {@link PublicKey} or its string form.
   * @param options.signal Abort signal to stop watching and end the iterable.
   *
   * ```ts
   * for await (const path of node.pathChanges(peerId)) {
   *   console.log(path.relay ? "relay" : "direct", path.active, path.addr);
   * }
   * ```
   */
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
        let stopped = false;
        let abortListener: (() => void) | undefined;
        const cleanup = async (): Promise<void> => {
          if (stopped) return;
          stopped = true;
          if (abortListener) {
            signal?.removeEventListener("abort", abortListener);
            abortListener = undefined;
          }
          try {
            await adapter.unsubscribePathChanges(endpointHandle, nodeId);
          } catch {
            // Endpoint shutdown may race iterator cleanup; the subscription is
            // already gone in that case.
          }
        };
        abortListener = () => {
          void cleanup();
        };
        signal?.addEventListener("abort", abortListener, { once: true });

        return {
          async next(): Promise<IteratorResult<PathInfo>> {
            if (stopped || signal?.aborted) {
              await cleanup();
              return { done: true, value: undefined };
            }
            const path = await adapter.nextPathChange(endpointHandle, nodeId);
            if (path === null || stopped || signal?.aborted) {
              await cleanup();
              return { done: true, value: undefined };
            }
            return { done: false, value: path };
          },
          async return(): Promise<IteratorResult<PathInfo>> {
            await cleanup();
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

  /**
   * Shut the node down: close the endpoint, drop all connections and sessions,
   * and stop the transport event loop. Resolves once shutdown is complete, after
   * which the {@link IrohNode.closed} promise also resolves.
   *
   * The node implements `Symbol.asyncDispose`, so `await using node = ...` calls
   * this automatically when the binding goes out of scope.
   *
   * @param options.force When `true`, tear down immediately without gracefully
   *   draining in-flight work.
   */
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

/**
 * An {@link IrohNode} whose `secretKey` is guaranteed to be present.
 *
 * `createNode` returns this refined type when — and only when — you pass a
 * `key`. Because you supplied the key, the node can hand it back via
 * `secretKey` without an `undefined` check. Omit `key` and you get the base
 * {@link IrohNode}, where `secretKey` is `SecretKey | undefined` — always
 * `undefined` at runtime, since a natively generated identity is never
 * exported to JS. This holds identically on Node.js, Deno, and Tauri.
 *
 * The intersection narrows `SecretKey | undefined` ∩ `SecretKey` → `SecretKey`,
 * so the common case keeps a non-optional `secretKey` while options types stay
 * fully shared.
 */
export type IrohNodeWithSecret = IrohNode & { readonly secretKey: SecretKey };
