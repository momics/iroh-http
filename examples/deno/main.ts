/**
 * iroh-http Deno example.
 *
 *   deno task server                  # serve HTTP over iroh
 *   deno task host                    # host the ./public directory over httpi://
 *   deno task client <peer-id>        # fetch from a server
 *   deno task advertise [service]     # announce this node on the LAN via mDNS
 *   deno task browse    [service]     # discover nodes on the LAN via mDNS
 *   deno task dnssd-advertise         # advertise ANY service (custom TXT/port)
 *   deno task dnssd-browse            # browse ANY service, print full records
 *
 * The `host` task wires Deno's standard-library file server (`serveDir`) onto an
 * httpi:// endpoint, so any peer can stream the files in ./public (audio.wav,
 * image.png) directly — e.g. from the Tauri example's "Stream files" tab. Those
 * assets are procedurally generated and in the public domain.
 *
 * To test mDNS, open two terminals on the same network:
 *   Terminal A:  deno task advertise
 *   Terminal B:  deno task browse
 * Terminal B prints terminal A's node ID as it is discovered. Press Ctrl+C to
 * stop either side. Pass a matching service name to scope discovery, e.g.
 *   deno task advertise my-app   &&   deno task browse my-app
 *
 * The `dnssd-*` tasks use the generic `node.advertise()` / `node.browse()`
 * surface to advertise and browse arbitrary DNS-SD services — with a custom
 * instance label, port, TXT records and protocol — not just iroh nodes.
 * `asIrohPeer` reinterprets any record that carries an iroh public key as a
 * peer. (`node.advertisePeer()` / `node.browsePeers()` are the iroh-http
 * specializations used by the `advertise` / `browse` tasks above.)
 */

import { asIrohPeer, createNode } from "@momics/iroh-http-deno";
import { serveDir } from "@std/http/file-server";
import { fromFileUrl } from "@std/path";

/** Default mDNS service name shared by `advertise` and `browse`. */
const DEFAULT_SERVICE = "iroh-http-example";

/** Default generic service name shared by the `dnssd-*` tasks. */
const DEFAULT_DNSSD_SERVICE = "demo-printer";

/** Read an env var without throwing when `--allow-env` was not granted. */
function envVar(name: string): string | undefined {
  try {
    return Deno.env.get(name);
  } catch {
    return undefined;
  }
}

/** Parse a comma-separated list of allowed peer IDs into a set. */
function parseAllowlist(raw: string | undefined): Set<string> {
  return new Set((raw ?? "").split(",").map((s) => s.trim()).filter(Boolean));
}

const [mode, arg] = Deno.args;

const node = await createNode();
console.log("Node ID:", node.publicKey.toString());

switch (mode) {
  case "server": {
    // Only peers in IROH_HTTP_ALLOWED_PEERS (comma-separated node IDs) may be
    // served. The `peer-id` header is the authenticated QUIC identity set by
    // the library. With no allowlist this demo serves anyone — never do that in
    // a real app.
    const allowed = parseAllowlist(envVar("IROH_HTTP_ALLOWED_PEERS"));
    if (allowed.size === 0) {
      console.warn(
        "[example] IROH_HTTP_ALLOWED_PEERS not set — serving ANY peer. " +
          "Set it to a comma-separated list of node IDs to restrict access.",
      );
    }
    node.serve({}, (req) => {
      const peer = req.headers.get("peer-id") ?? "";
      if (allowed.size > 0 && !allowed.has(peer)) {
        console.warn("Rejected request from non-allowlisted peer:", peer);
        return new Response("Forbidden", { status: 403 });
      }
      const path = new URL(req.url).pathname;
      console.log("Incoming:", req.method, path);
      return new Response(`Hello from Deno iroh-http! Path: ${path}`);
    });
    console.log("Serving. Share your node ID with the client.");
    break;
  }

  case "host": {
    // Host the ./public directory over httpi:// using Deno's vanilla std file
    // server. `serveDir` handles content types, ETags, and Range requests (so
    // audio/video seek), and our serve handler simply forwards each request to
    // it. Same access-control note as `server` applies.
    const allowed = parseAllowlist(envVar("IROH_HTTP_ALLOWED_PEERS"));
    if (allowed.size === 0) {
      console.warn(
        "[example] IROH_HTTP_ALLOWED_PEERS not set — serving ANY peer. " +
          "Set it to a comma-separated list of node IDs to restrict access.",
      );
    }
    const fsRoot = fromFileUrl(new URL("./public", import.meta.url));
    node.serve({}, (req) => {
      const peer = req.headers.get("peer-id") ?? "";
      if (allowed.size > 0 && !allowed.has(peer)) {
        console.warn("Rejected request from non-allowlisted peer:", peer);
        return new Response("Forbidden", { status: 403 });
      }
      console.log(
        "Incoming:",
        req.method,
        new URL(req.url).pathname,
        "←",
        peer.slice(0, 16),
      );
      return serveDir(req, { fsRoot, quiet: true });
    });
    console.log(`Hosting ${fsRoot} over httpi://.`);
    console.log("Available files: /audio.wav, /image.png");
    console.log("Share your node ID with the client (e.g. the Tauri example).");
    break;
  }

  case "client": {
    if (!arg) {
      console.error("Usage: deno task client <peer-id>");
      Deno.exit(1);
    }
    const res = await node.fetch(`httpi://${arg}/hello`);
    console.log("Status:", res.status);
    console.log("Body:", await res.text());
    await node.close();
    break;
  }

  case "advertise": {
    const serviceName = arg ?? DEFAULT_SERVICE;
    const abort = new AbortController();
    Deno.addSignalListener("SIGINT", () => abort.abort());
    console.log(`Advertising as "${serviceName}". Press Ctrl+C to stop.`);
    await node.advertisePeer({ serviceName, signal: abort.signal });
    await node.close();
    break;
  }

  case "browse": {
    const serviceName = arg ?? DEFAULT_SERVICE;
    const abort = new AbortController();
    Deno.addSignalListener("SIGINT", () => abort.abort());
    console.log(`Browsing for "${serviceName}". Press Ctrl+C to stop.`);
    for await (
      const peer of node.browsePeers({ serviceName, signal: abort.signal })
    ) {
      const status = peer.isActive ? "discovered" : "expired";
      const addrs = peer.addrs.length > 0 ? `  [${peer.addrs.join(", ")}]` : "";
      console.log(`  ${status}: ${peer.nodeId}${addrs}`);
    }
    await node.close();
    break;
  }

  case "dnssd-advertise": {
    // Advertise an arbitrary DNS-SD service — not an iroh node. Everything is
    // caller-controlled: instance label, SRV port, TXT records and protocol.
    const serviceName = arg ?? DEFAULT_DNSSD_SERVICE;
    const abort = new AbortController();
    Deno.addSignalListener("SIGINT", () => abort.abort());
    console.log(
      `Advertising "Front Desk Printer" as _${serviceName}._tcp on :9100. ` +
        "Press Ctrl+C to stop.",
    );
    await node.advertise({
      serviceName,
      instanceName: "Front Desk Printer",
      port: 9100,
      protocol: "tcp",
      txt: { model: "LaserJet 9000", color: "true", pdl: "application/pdf" },
      signal: abort.signal,
    });
    await node.close();
    break;
  }

  case "dnssd-browse": {
    // Browse any DNS-SD service and print the full, lossless record. Records
    // that carry an iroh public key are additionally surfaced as peers.
    const serviceName = arg ?? DEFAULT_DNSSD_SERVICE;
    const abort = new AbortController();
    Deno.addSignalListener("SIGINT", () => abort.abort());
    console.log(`Browsing for _${serviceName}. Press Ctrl+C to stop.`);
    for await (
      const record of node.browse({
        serviceName,
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
    break;
  }

  default:
    console.error(
      "Usage: deno task server | host | client <peer-id> | advertise [service] | browse [service] | dnssd-advertise [service] | dnssd-browse [service]",
    );
    Deno.exit(1);
}
