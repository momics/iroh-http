/**
 * Cross-runtime compliance server — Deno.
 *
 * Mirror of server.mjs but using the Deno FFI adapter.
 * Prints "READY:<json>" to stdout when ready, then serves until SIGINT.
 */

import { createNode } from "../../packages/iroh-http-deno/mod.ts";
import { handleRequest } from "./handler.mjs";

const node = await createNode();
const { id: nodeId, addrs } = await node.addr();

const ac = new AbortController();
node.serve({ signal: ac.signal }, handleRequest);

// Signal readiness.
Deno.stdout.writeSync(
  new TextEncoder().encode("READY:" + JSON.stringify({ nodeId, addrs }) + "\n"),
);
Deno.stderr.writeSync(
  new TextEncoder().encode(`[server.deno.ts] serving as ${nodeId}\n`),
);

// Wait for signal.
Deno.addSignalListener("SIGINT", async () => {
  ac.abort();
  await node.close();
  Deno.exit(0);
});
Deno.addSignalListener("SIGTERM", async () => {
  ac.abort();
  await node.close();
  Deno.exit(0);
});

await new Promise(() => {});
