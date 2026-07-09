/**
 * iroh-http-deno — public API.
 *
 * ```ts
 * import { createNode } from "@momics/iroh-http-deno";
 *
 * const node = await createNode({ key: savedKey });
 * const server = node.serve(req => new Response("hello"));
 * await server.finished;
 * const res = await node.fetch(peerId.toURL("/api"));
 * ```
 */

import {
  IrohNode,
  type IrohNodeWithSecret,
  type NodeOptions,
  type SecretKey,
} from "@momics/iroh-http-shared";
import {
  createEndpointInfo,
  DenoAdapter,
  generateSecretKey,
  publicKeyVerify,
  secretKeySign,
  waitEndpointClosed,
} from "./src/adapter.ts";
export { generateSecretKey, publicKeyVerify, secretKeySign };
export {
  asIrohPeer,
  IROH_HTTP_SERVICE,
  PublicKey,
  SecretKey,
  TXT_KEY_PUBLIC_KEY,
  TXT_KEY_RELAY,
} from "@momics/iroh-http-shared";
export type {
  DnsSdAdvertiseOptions,
  DnsSdBrowseOptions,
  DnsSdProtocol,
  ServiceConfig,
  ServiceRecord,
} from "@momics/iroh-http-shared";

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
 * import { createNode } from "@momics/iroh-http-deno";
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
  const info = await createEndpointInfo(options);
  const adapter = new DenoAdapter(info.endpointHandle);
  return IrohNode._create(
    adapter,
    info,
    options,
    waitEndpointClosed(info.endpointHandle),
  );
}

export type { IrohNode, IrohNodeWithSecret, NodeOptions };
