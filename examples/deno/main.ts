/**
 * iroh-http Deno example.
 *
 *   deno task server                  # serve HTTP over iroh
 *   deno task client <peer-id>        # fetch from a server
 *   deno task advertise [service]     # announce this node on the LAN via mDNS
 *   deno task browse    [service]     # discover nodes on the LAN via mDNS
 *
 * To test mDNS, open two terminals on the same network:
 *   Terminal A:  deno task advertise
 *   Terminal B:  deno task browse
 * Terminal B prints terminal A's node ID as it is discovered. Press Ctrl+C to
 * stop either side. Pass a matching service name to scope discovery, e.g.
 *   deno task advertise my-app   &&   deno task browse my-app
 */

import { createNode } from "@momics/iroh-http-deno";

/** Default mDNS service name shared by `advertise` and `browse`. */
const DEFAULT_SERVICE = "iroh-http-example";

const [mode, arg] = Deno.args;

const node = await createNode();
console.log("Node ID:", node.publicKey.toString());

switch (mode) {
  case "server": {
    node.serve({}, (req) => {
      const path = new URL(req.url).pathname;
      console.log("Incoming:", req.method, path);
      return new Response(`Hello from Deno iroh-http! Path: ${path}`);
    });
    console.log("Serving. Share your node ID with the client.");
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
    await node.advertise({ serviceName, signal: abort.signal });
    await node.close();
    break;
  }

  case "browse": {
    const serviceName = arg ?? DEFAULT_SERVICE;
    const abort = new AbortController();
    Deno.addSignalListener("SIGINT", () => abort.abort());
    console.log(`Browsing for "${serviceName}". Press Ctrl+C to stop.`);
    for await (
      const peer of node.browse({ serviceName, signal: abort.signal })
    ) {
      const status = peer.isActive ? "discovered" : "expired";
      const addrs = peer.addrs.length > 0 ? `  [${peer.addrs.join(", ")}]` : "";
      console.log(`  ${status}: ${peer.nodeId}${addrs}`);
    }
    await node.close();
    break;
  }

  default:
    console.error(
      "Usage: deno task server | client <peer-id> | advertise [service] | browse [service]",
    );
    Deno.exit(1);
}
