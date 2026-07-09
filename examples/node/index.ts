/**
 * iroh-http Node.js example.
 *
 * Run two instances of this script:
 *   node 1: npm start -- server
 *   node 2: npm start -- client <node1-id>
 *
 * Generic DNS-SD (advertise/browse ANY local service, not just iroh nodes):
 *   node 1: npm start -- dnssd-advertise
 *   node 2: npm start -- dnssd-browse
 */

import { asIrohPeer, createNode } from "@momics/iroh-http-node";

const [mode, peerId] = process.argv.slice(2);

/** Default generic service name shared by the `dnssd-*` modes. */
const DNSSD_SERVICE = "demo-printer";

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
} else if (mode === "dnssd-advertise") {
  // Advertise an arbitrary DNS-SD service — not an iroh node. The instance
  // label, SRV port, TXT records and protocol are all caller-controlled.
  const abort = new AbortController();
  process.on("SIGINT", () => abort.abort());
  console.log(
    `Advertising "Front Desk Printer" as _${DNSSD_SERVICE}._tcp on :9100. ` +
      "Press Ctrl+C to stop.",
  );
  await node.dnsSd.advertise({
    serviceName: DNSSD_SERVICE,
    instanceName: "Front Desk Printer",
    port: 9100,
    protocol: "tcp",
    txt: { model: "LaserJet 9000", color: "true", pdl: "application/pdf" },
    signal: abort.signal,
  });
  await node.close();
} else if (mode === "dnssd-browse") {
  // Browse any DNS-SD service and print the full, lossless record. Records
  // that carry an iroh public key are additionally surfaced as peers.
  const abort = new AbortController();
  process.on("SIGINT", () => abort.abort());
  console.log(`Browsing for _${DNSSD_SERVICE}. Press Ctrl+C to stop.`);
  for await (
    const record of node.dnsSd.browse({
      serviceName: DNSSD_SERVICE,
      protocol: "tcp",
      signal: abort.signal,
    })
  ) {
    const status = record.isActive ? "discovered" : "expired";
    const txt = Object.entries(record.txt)
      .map(([k, v]) => `${k}=${v}`)
      .join(", ");
    console.log(`  ${status}: ${record.instanceName} :${record.port}`);
    if (record.addrs.length > 0) {
      console.log(`      addrs: ${record.addrs.join(", ")}`);
    }
    if (txt) console.log(`      txt:   ${txt}`);
    const peer = asIrohPeer(record);
    if (peer) console.log(`      ↳ iroh peer: ${peer.nodeId}`);
  }
  await node.close();
} else {
  console.error(
    "Usage: tsx index.ts server | client <peer-id> | dnssd-advertise | dnssd-browse",
  );
  process.exit(1);
}
