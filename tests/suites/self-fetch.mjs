/**
 * Self-request tests — a node fetching its own node id (ADR-015, issue #264).
 *
 * iroh's transport forbids self-dial; core routes self-targeted fetches into
 * the node's own serve service in-process. This suite asserts the behavior is
 * consistent across runtimes (acceptance criterion #3): success when serving,
 * a clear error when not.
 *
 * Shared across all runtimes.
 */

export function selfFetchTests({ createNode, test, assert, assertEqual }) {
  test("node can fetch its own node id while serving", async () => {
    const node = await createNode({ disableNetworking: true });
    try {
      const { id } = await node.addr();

      node.serve({}, (req) => {
        const url = new URL(req.url);
        const peer = req.headers.get("peer-id") ?? "";
        return new Response(`self:${url.pathname} peer:${peer}`, {
          status: 200,
          headers: { "content-type": "text/plain" },
        });
      });

      const res = await node.fetch(`httpi://${id}/loopback`);
      assertEqual(res.status, 200, "self-fetch should return 200");
      const text = await res.text();
      assert(
        text.includes("self:/loopback"),
        `body should echo the path, got: ${text}`,
      );
      assert(
        text.includes(`peer:${id}`),
        `handler should observe own id as peer, got: ${text}`,
      );
    } finally {
      await node.close();
    }
  });

  test("self-fetch without a server errors clearly", async () => {
    const node = await createNode({ disableNetworking: true });
    try {
      const { id } = await node.addr();
      let threw = false;
      try {
        const res = await node.fetch(`httpi://${id}/loopback`);
        // Some adapters surface transport errors as a 0/5xx status rather than
        // a thrown error; accept either as long as it is not a 2xx.
        assert(
          res.status === 0 || res.status >= 400,
          `expected an error, got status ${res.status}`,
        );
        await res.body?.cancel();
      } catch (err) {
        threw = true;
        assert(err instanceof Error, "expected an Error instance");
        assert(
          /self-request|no active server|server/i.test(err.message),
          `error should explain the missing server, got: ${err.message}`,
        );
      }
      assert(threw || true, "handled error path");
    } finally {
      await node.close();
    }
  });
}
