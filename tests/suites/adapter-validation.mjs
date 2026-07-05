/**
 * Adapter-boundary validation conformance tests.
 *
 * Shared across Node, Deno, and Tauri so malformed fetch header rows and fetch
 * numeric knobs fail closed identically at the adapter boundary.
 */

const MAX_TIMEOUT_MS = 300_000;
const MAX_BODY_BYTES = 16 * 1024 * 1024;

export function adapterValidationTests({
  createNode,
  test,
  assert,
  assertEqual,
  assertThrows,
}) {
  test("adapter validation rejects malformed fetch header rows", async () => {
    const node = await createNode({ disableNetworking: true });
    try {
      const { id } = await node.addr();
      const url = `httpi://${id}/validation`;
      const cases = [
        { name: "row length != 2", headers: [["x-bad"]] },
        {
          name: "over-long name",
          headers: [["x".repeat(257), "value"]],
        },
        {
          name: "over-long value",
          headers: [["x-test", "v".repeat(8193)]],
        },
        {
          name: "too many rows",
          headers: Array.from({ length: 101 }, (_, i) => [`x-${i}`, "v"]),
        },
      ];

      for (const c of cases) {
        await assertThrows(async () => {
          await node.fetch(url, { headers: c.headers });
        }, c.name);
      }
    } finally {
      await node.close();
    }
  });

  test("adapter validation rejects invalid fetch numeric knobs", async () => {
    const node = await createNode({ disableNetworking: true });
    try {
      const { id } = await node.addr();
      const url = `httpi://${id}/validation`;
      const timeoutValues = [
        Number.NaN,
        Number.POSITIVE_INFINITY,
        -1,
        1.5,
        MAX_TIMEOUT_MS + 1,
      ];
      const bodyValues = [
        Number.NaN,
        Number.POSITIVE_INFINITY,
        -1,
        1.5,
        MAX_BODY_BYTES + 1,
      ];

      for (const value of timeoutValues) {
        await assertThrows(async () => {
          await node.fetch(url, { requestTimeout: value });
        }, `requestTimeout=${String(value)}`);
      }
      for (const value of bodyValues) {
        await assertThrows(async () => {
          await node.fetch(url, { maxResponseBodyBytes: value });
        }, `maxResponseBodyBytes=${String(value)}`);
      }
    } finally {
      await node.close();
    }
  });

  test("adapter validation accepts valid fetch headers and numeric knobs", async () => {
    const node = await createNode({ disableNetworking: true });
    let handle;
    try {
      const { id } = await node.addr();
      handle = node.serve({}, (req) => {
        return new Response(req.headers.get("x-conformance") ?? "", {
          headers: [["x-conformance-response", "ok"]],
        });
      });

      const res = await node.fetch(`httpi://${id}/validation`, {
        headers: [["x-conformance", "ok"]],
        requestTimeout: 30_000,
        maxResponseBodyBytes: 1024,
      });

      assertEqual(res.status, 200, "valid request should succeed");
      assertEqual(await res.text(), "ok", "valid header should pass through");
      assertEqual(
        res.headers.get("x-conformance-response"),
        "ok",
        "valid response header should pass through",
      );
    } finally {
      await node.close();
      if (handle) await handle.finished.catch(() => {});
    }
  });

  test("adapter validation preserves default fetch knobs", async () => {
    const node = await createNode({ disableNetworking: true });
    let handle;
    try {
      const { id } = await node.addr();
      handle = node.serve(() => new Response("default-ok"));
      const res = await node.fetch(`httpi://${id}/defaults`);

      assert(res.ok, `defaulted fetch should succeed, got ${res.status}`);
      assertEqual(await res.text(), "default-ok", "defaulted fetch body");
    } finally {
      await node.close();
      if (handle) await handle.finished.catch(() => {});
    }
  });
}
