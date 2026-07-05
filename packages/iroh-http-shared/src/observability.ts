/**
 * Detail payload for the `"peerconnect"` event dispatched on `IrohNode` when
 * a peer establishes its first QUIC connection to a running serve loop.
 */
export interface PeerConnectEventDetail {
  /** Base32-encoded public key of the connecting peer. */
  nodeId: string;
}

/**
 * Detail payload for the `"peerdisconnect"` event dispatched on `IrohNode`
 * when a peer's last QUIC connection to the serve loop closes.
 */
export interface PeerDisconnectEventDetail {
  /** Base32-encoded public key of the disconnecting peer. */
  nodeId: string;
}

/**
 * Detail payload for the `"pathchange"` event dispatched on `IrohNode` when
 * the QUIC path to a connected peer changes (e.g. relay → direct, or new
 * network interface selected).
 */
export interface PathChangeEventDetail {
  /** Base32-encoded public key of the peer whose path changed. */
  nodeId: string;
  /** Whether the active path goes through a relay server. */
  relay: boolean;
  /** Remote address string, e.g. `"1.2.3.4:5678"` or relay URL. */
  addr: string;
  /** Unix epoch timestamp in milliseconds. */
  timestamp: number;
}

/**
 * Detail payload for the `"diagnostics"` event dispatched on `IrohNode` for
 * connection-pool and handle-lifecycle events.
 *
 * `kind` identifies the event class:
 * - `"pool:hit"` — an existing pooled connection was reused.
 * - `"pool:miss"` — no pooled connection was available; a new one was opened.
 * - `"pool:evict"` — an idle connection was evicted from the pool.
 * - `"handle:sweep"` — expired body/request handles were garbage-collected.
 *
 * Additional properties vary by `kind`; cast or use `in` to narrow.
 */
export interface DiagnosticsEventDetail {
  kind: string;
  [key: string]: unknown;
}

/** @internal Transport-level event used by platform adapters. */
export type TransportEventPayload =
  | { type: "pool:hit"; peerId: string; timestamp: number }
  | { type: "pool:miss"; peerId: string; timestamp: number }
  | { type: "pool:evict"; peerId: string; timestamp: number }
  | {
    type: "path:change";
    peerId: string;
    addr: string;
    relay: boolean;
    timestamp: number;
  }
  | { type: "handle:sweep"; evicted: number; timestamp: number };

export interface EndpointStats {
  activeReaders: number;
  activeWriters: number;
  activeSessions: number;
  totalHandles: number;
  poolSize: number;
  activeConnections: number;
  activeRequests: number;
  activePathSubscriptions: number;
  activePathWatchers: number;
}

export interface PeerStats {
  relay: boolean;
  relayUrl: string | null;
  paths: PathInfo[];
  rttMs: number | null;
  bytesSent: number | null;
  bytesReceived: number | null;
  lostPackets: number | null;
  sentPackets: number | null;
  congestionWindow: number | null;
}

export interface PathInfo {
  relay: boolean;
  addr: string;
  active: boolean;
}
