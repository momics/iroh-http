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
 * TXT key carrying a dialable direct `ip:port` address.
 *
 * Advertisers whose SRV port is not the real QUIC port — notably the iOS
 * native `NWListener`, whose UDP port is unrelated to the QUIC socket, and
 * whose resolved A-record host carries no port at all — publish the dialable
 * `ip:<bound QUIC port>` here instead. See #346.
 */
export const TXT_KEY_ADDRESS = "address";

/**
 * Whether `s` is a well-formed, dialable `ip:port` socket address.
 *
 * Rejects bare IPs (no port), an explicit `:0` port, and anything else that
 * cannot be dialed. A bare A-record host (`192.168.50.227`) or a `:0` address
 * would otherwise fail `parse_direct_addrs` at dial time and poison the whole
 * direct-address list (#346).
 */
export function isDialableSocketAddr(s: string): boolean {
  const idx = s.lastIndexOf(":");
  if (idx <= 0 || idx === s.length - 1) return false;
  const host = s.slice(0, idx);
  const portStr = s.slice(idx + 1);
  // Strict decimal 1–5 digits. `Number()` would otherwise accept "0x1a",
  // "4e2", "+443", " 443", "443 " — the raw, unparseable string would then
  // reach the dialer, where `parse_direct_addrs` errors and (all-or-nothing)
  // drops the ENTIRE direct-address list, leaving the peer relay-only (#350).
  if (!/^[0-9]{1,5}$/.test(portStr)) return false;
  const port = Number(portStr);
  if (port < 1 || port > 65535) return false;
  // Bracketed IPv6 (`[::1]:443`) or a host with no stray colon, bracket, or
  // whitespace (IPv4 / hostname). A bare, unbracketed IPv6 has multiple colons
  // and no brackets — reject it as ambiguous rather than mis-parse it.
  if (host.startsWith("[")) {
    return host.endsWith("]") && host.length > 2 && !/[\s]/.test(host);
  }
  return host.length > 0 && !/[:[\]\s]/.test(host);
}

/**
 * Whether `s` looks like a relay URL rather than a direct socket address.
 *
 * Relay URLs (`https://relay.example`) are valid dialing input — a peer behind
 * NAT or off the LAN is only reachable via its home relay — but they are not
 * `ip:port` and must bypass {@link isDialableSocketAddr}. Kept deliberately
 * permissive: anything with an `http(s)` (or `relay`) scheme is a relay.
 */
export function isRelayUrl(s: string): boolean {
  return /^(https?|relay):\/\//i.test(s);
}

/**
 * Interpret a generic {@link ServiceRecord} as an iroh-http {@link DiscoveredPeer}.
 *
 * Reads the peer's public key from the `pk` TXT property, falling back to the
 * instance label. Returns `null` when neither yields a node id.
 *
 * Addresses are sanitised for the dialer (#346): the dialable `address` TXT
 * entry (which carries the real bound QUIC port) is surfaced first, then any
 * well-formed `ip:port` socket addresses from the resolved set. Bare,
 * port-less hosts (an iOS A-record resolves to a portless IP) and `:0`
 * addresses are dropped so they can never fail `parse_direct_addrs` and poison
 * the whole list.
 *
 * Relay URLs are preserved (the `relay` TXT entry and any relay-scheme entry in
 * `addrs`) so an off-LAN / NAT'd peer stays reachable via its home relay —
 * the `ip:port` hardening applies only to direct addresses (#350 review W3).
 */
export function asIrohPeer(record: ServiceRecord): DiscoveredPeer | null {
  const nodeId = record.txt[TXT_KEY_PUBLIC_KEY] ?? record.instanceName;
  if (!nodeId) return null;

  const addrs: string[] = [];
  const push = (a: string | undefined) => {
    if (!a || addrs.includes(a)) return;
    // Relay URLs are dialable but are not `ip:port`; keep them verbatim.
    if (isRelayUrl(a) || isDialableSocketAddr(a)) addrs.push(a);
  };

  push(record.txt[TXT_KEY_ADDRESS]);
  push(record.txt[TXT_KEY_RELAY]);
  for (const a of record.addrs) push(a);

  return { nodeId, addrs, isActive: record.isActive };
}
