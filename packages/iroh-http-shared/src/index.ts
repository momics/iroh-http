export type {
  BidirectionalStream,
  CloseOptions,
  EndpointInfo,
  FetchOptions,
  FfiDuplexStream,
  FfiServeOptions,
  IrohFetchInit,
  NodeAddrInfo,
  PeerConnectionEvent,
} from "./IrohAdapter.js";
export { IrohAdapter } from "./IrohAdapter.js";
export { IrohNode } from "./IrohNode.js";
export type { IrohNodeWithSecret } from "./IrohNode.js";
export type { NodeOptions, RelayMode } from "./options/NodeOptions.js";
export type {
  DiagnosticsEventDetail,
  EndpointStats,
  PathChangeEventDetail,
  PathInfo,
  PeerConnectEventDetail,
  PeerDisconnectEventDetail,
  PeerStats,
} from "./observability.js";
export type {
  AdvertiseOptions,
  BrowseOptions,
  DiscoveredPeer,
  PeerDiscoveryEvent,
} from "./discovery.js";
export type {
  IrohSession,
  RawSessionFns,
  WebTransportBidirectionalStream,
  WebTransportCloseInfo,
  WebTransportDatagramDuplexStream,
} from "./session.js";
export type {
  ServeFn,
  ServeHandle,
  ServeHandler,
  ServeOptions,
} from "./serve.js";
export { buildSession } from "./session.js";
export { bodyInitToStream, makeReadable, pipeToWriter } from "./streams.js";
export { makeFetch } from "./fetch.js";
export { makeServe } from "./serve.js";
export { PublicKey, resolveNodeId } from "./PublicKey.js";
export { SecretKey } from "./SecretKey.js";
export {
  classifyBindError,
  classifyError,
  IrohAbortError,
  IrohArgumentError,
  IrohBindError,
  IrohConnectError,
  IrohError,
  IrohHandleError,
  IrohProtocolError,
  IrohStreamError,
  isEndpointGoneError,
} from "./errors.js";
export {
  bigintToSafeNumber,
  decodeBase64,
  encodeBase64,
  normaliseCompression,
  normaliseDiscovery,
  normaliseRelayMode,
} from "./utils.js";
export type {
  NormalisedCompression,
  NormalisedDiscovery,
  NormalisedRelay,
} from "./utils.js";

export function ticketNodeId(ticket: string): string {
  try {
    const info = JSON.parse(ticket) as { id?: string };
    if (info && typeof info.id === "string") return info.id;
  } catch {}
  return ticket;
}
