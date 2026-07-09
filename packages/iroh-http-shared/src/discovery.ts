/** A peer found (or lost) during mDNS/DNS-SD browsing. */
export interface DiscoveredPeer {
  /** Base32 public key of the discovered peer. */
  nodeId: string;
  /** Network addresses (e.g. QUIC socket addrs) reported for this peer. */
  addrs: string[];
  /** `true` while the peer is reachable; `false` after it expires. */
  isActive: boolean;
}

/** Options for `node.browsePeers()` — discover peers on the local network. */
export interface BrowseOptions {
  /** mDNS service name to browse. Defaults to the iroh-http service type. */
  serviceName?: string;
  /** Abort signal to stop browsing. */
  signal?: AbortSignal;
}

/** Options for `node.advertisePeer()` — announce this node on the local network. */
export interface AdvertiseOptions {
  /** mDNS service name to advertise under. Defaults to the iroh-http service type. */
  serviceName?: string;
  /** Abort signal to stop advertising. */
  signal?: AbortSignal;
}

/** Event emitted by `node.browsePeers()` when a peer is discovered or expires. */
export interface PeerDiscoveryEvent {
  /** Whether the peer was just discovered or has expired. */
  type: "discovered" | "expired";
  /** Base32 public key of the peer. */
  nodeId: string;
  /** Network addresses, present on `"discovered"` events. */
  addrs?: string[];
}

// ── Generic DNS-SD ────────────────────────────────────────────────────────────

/** DNS-SD transport protocol. Defaults to `"udp"`. */
export type DnsSdProtocol = "udp" | "tcp";

/**
 * A generic DNS-SD service to advertise via `node.advertise()`.
 *
 * This is the lossless, protocol-neutral shape underneath iroh-http's own
 * discovery. Use it to announce any local service, not just iroh nodes.
 */
export interface ServiceConfig {
  /** Bare service name, e.g. `"my-app"` → `_my-app._udp.local.`. */
  serviceName: string;
  /** DNS-SD instance label that uniquely names this advertisement. */
  instanceName: string;
  /** SRV port peers connect to. Must fit an unsigned 16-bit integer (0–65535). */
  port: number;
  /** TXT key/value properties. Defaults to none. */
  txt?: Record<string, string>;
  /**
   * Extra IP addresses to advertise. Local interface addresses are added
   * automatically, so this is usually left empty.
   */
  addrs?: string[];
  /** Transport protocol. Defaults to `"udp"`. */
  protocol?: DnsSdProtocol;
}

/** Options for `node.advertise()`. */
export interface DnsSdAdvertiseOptions extends ServiceConfig {
  /** Abort signal to stop advertising. */
  signal?: AbortSignal;
}

/** Options for `node.browse()`. */
export interface DnsSdBrowseOptions {
  /** Bare service name to browse, e.g. `"my-app"`. */
  serviceName: string;
  /** Transport protocol. Defaults to `"udp"`. */
  protocol?: DnsSdProtocol;
  /** Abort signal to stop browsing and end the iterable. */
  signal?: AbortSignal;
}

/**
 * A fully-resolved DNS-SD service record yielded by `node.browse()`.
 *
 * Records are lossless: every TXT property, the instance label, host, port and
 * resolved socket addresses are preserved.
 *
 * **Platform caveat:** on iOS, the generic `node.browse()` path is
 * metadata-only — `host` is `undefined`, `port` is `0`, and `addrs` is empty,
 * even on `"discovered"` (active) records. Resolving the target host/port/IP
 * would require opening an `NWConnection` per result, which this surface does
 * not do. Desktop and Android fully resolve these fields. See ADR-018 §8 for
 * the rationale; consumers that need addresses on iOS should rely on a
 * `relay`/`address` TXT entry surfaced through `addrs` as a best effort, or
 * use `asIrohPeer()` / `node.browsePeers()` for the iroh-peer specialization,
 * which resolves addresses via `AddressLookup` instead of DNS-SD.
 */
export interface ServiceRecord {
  /** `true` when the service appeared or was updated; `false` on expiry. */
  isActive: boolean;
  /** Service type and domain, e.g. `_my-app._udp.local.`. */
  serviceType: string;
  /** Service instance label. */
  instanceName: string;
  /** Host name. Absent on expiry. */
  host?: string;
  /** SRV port. `0` on expiry. */
  port: number;
  /** Resolved socket addresses. Empty on expiry. */
  addrs: string[];
  /** All TXT key/value properties. Empty on expiry. */
  txt: Record<string, string>;
}

/** DNS-SD service name used by iroh-http's own peer discovery. */
export const IROH_HTTP_SERVICE = "iroh-http";

/** TXT key carrying the base32 public key in iroh-http records. */
export const TXT_KEY_PUBLIC_KEY = "pk";

/** TXT key carrying the home relay URL in iroh-http records. */
export const TXT_KEY_RELAY = "relay";

/**
 * Interpret a generic {@link ServiceRecord} as an iroh-http {@link DiscoveredPeer}.
 *
 * Reads the peer's public key from the `pk` TXT property, falling back to the
 * instance label. Returns `null` when neither yields a node id.
 */
export function asIrohPeer(record: ServiceRecord): DiscoveredPeer | null {
  const nodeId = record.txt[TXT_KEY_PUBLIC_KEY] ?? record.instanceName;
  if (!nodeId) return null;
  return { nodeId, addrs: record.addrs, isActive: record.isActive };
}
