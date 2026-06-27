/**
 * iroh-http Node.js example.
 *
 * Run two instances of this script:
 *   node 1: npm start -- server
 *   node 2: npm start -- client <node1-id>
 */

import { createNode } from "@momics/iroh-http-node";

const [mode, peerId] = process.argv.slice(2);

/** Parse a comma-separated list of allowed peer IDs into a set. */
function parseAllowlist(raw: string | undefined): Set<string> {
  return new Set((raw ?? "").split(",").map((s) => s.trim()).filter(Boolean));
}

const node = await createNode();
console.log("Node ID:", node.publicKey.toString());

if (mode === "server") {
  // Only peers in IROH_HTTP_ALLOWED_PEERS (comma-separated node IDs) may be
  // served. The `peer-id` header is the authenticated QUIC identity set by the
  // library, not client-controlled. With no allowlist this demo serves anyone —
  // never do that in a real app.
  const allowed = parseAllowlist(process.env.IROH_HTTP_ALLOWED_PEERS);
  if (allowed.size === 0) {
    console.warn(
      "[example] IROH_HTTP_ALLOWED_PEERS not set — serving ANY peer. " +
        "Set it to a comma-separated list of node IDs to restrict access.",
    );
  }
  node.serve({}, async (req) => {
    const peer = req.headers.get("peer-id") ?? "";
    if (allowed.size > 0 && !allowed.has(peer)) {
      console.warn("Rejected request from non-allowlisted peer:", peer);
      return new Response("Forbidden", { status: 403 });
    }
    const path = new URL(req.url).pathname;
    console.log("Incoming request:", req.method, path);
    return new Response(`Hello from iroh-http! Path: ${path}`, {
      headers: { "content-type": "text/plain" },
    });
  });
  console.log("Serving. Share your node ID with the client.");
} else if (mode === "client" && peerId) {
  const res = await node.fetch(`httpi://${peerId}/hello`);
  console.log("Response status:", res.status);
  console.log("Body:", await res.text());
  await node.close();
} else {
  console.error("Usage: tsx index.ts server | tsx index.ts client <peer-id>");
  process.exit(1);
}
