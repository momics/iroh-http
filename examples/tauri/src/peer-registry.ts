/**
 * Shared in-memory peer registry for the dev console.
 *
 * Every place that discovers or targets a peer — Discovery-tab browse, generic
 * DNS-SD browse, testing-mode browse, a manual node-id/ticket paste, and a
 * session connect — upserts into this single store keyed by node id. The
 * reusable peer-picker (see `peer-picker.ts`) reads it so a peer discovered in
 * one tab can be targeted from any other tab without copy-pasting ids.
 */

/** Where a peer entry was last learned from. */
export type PeerSource =
  | "discovery"
  | "generic"
  | "test"
  | "manual"
  | "session";

export interface RegistryPeer {
  /** Base32 public key. */
  nodeId: string;
  /** Short human label (defaults to a truncated node id). */
  label: string;
  /** Best-known platform (`android` / `ios` / `desktop` / `?`). */
  platform: string;
  /** Union of dialable addresses (`ip:port` and/or relay URLs) seen so far. */
  addrs: string[];
  /** The source of the most recent update. */
  source: PeerSource;
  /** Epoch ms of the last update. */
  lastSeen: number;
}

export interface PeerUpsert {
  nodeId: string;
  source: PeerSource;
  label?: string;
  platform?: string;
  addrs?: string[];
}

type Listener = (peers: RegistryPeer[]) => void;

const peers = new Map<string, RegistryPeer>();
const listeners = new Set<Listener>();

function shortId(nodeId: string): string {
  return nodeId.length > 16 ? `${nodeId.slice(0, 16)}…` : nodeId;
}

function snapshot(): RegistryPeer[] {
  return [...peers.values()].sort((a, b) => b.lastSeen - a.lastSeen);
}

function emit(): void {
  const list = snapshot();
  for (const fn of listeners) {
    try {
      fn(list);
    } catch {
      /* a listener must never break the registry */
    }
  }
}

/**
 * Insert or merge a peer. Addresses are unioned (never lost), a concrete
 * platform overrides a placeholder `?`, and `lastSeen`/`source` reflect this
 * update. Returns the merged entry.
 */
export function upsertPeer(input: PeerUpsert): RegistryPeer {
  const nodeId = input.nodeId.trim();
  const existing = peers.get(nodeId);

  const addrs = new Set(existing?.addrs ?? []);
  for (const a of input.addrs ?? []) {
    if (a) addrs.add(a);
  }

  const platform = input.platform && input.platform !== "?"
    ? input.platform
    : existing?.platform ?? input.platform ?? "?";

  const merged: RegistryPeer = {
    nodeId,
    label: input.label ?? existing?.label ?? shortId(nodeId),
    platform,
    addrs: [...addrs],
    source: input.source,
    lastSeen: Date.now(),
  };
  peers.set(nodeId, merged);
  emit();
  return merged;
}

/** Remove a peer (e.g. a discovery `isActive=false` expiry). */
export function removePeer(nodeId: string): void {
  if (peers.delete(nodeId.trim())) emit();
}

export function getPeers(): RegistryPeer[] {
  return snapshot();
}

export function getPeer(nodeId: string): RegistryPeer | undefined {
  return peers.get(nodeId.trim());
}

/**
 * Subscribe to registry changes. Fires immediately with the current snapshot
 * and on every subsequent change. Returns an unsubscribe function.
 */
export function subscribe(fn: Listener): () => void {
  listeners.add(fn);
  fn(snapshot());
  return () => listeners.delete(fn);
}
