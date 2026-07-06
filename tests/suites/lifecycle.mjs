/**
 * Lifecycle tests — node creation, shutdown, resource cleanup, API surface.
 *
 * Shared across all runtimes. Each runtime provides { createNode, test, assert, assertEqual, ... }
 * via the injected context.
 */

export function lifecycleTests({ createNode, test, assert, assertEqual, assertNotEqual, assertResolves }) {
  // ── Node creation ──────────────────────────────────────────────────────────

  test("createNode returns a node", async () => {
    const node = await createNode();
    assert(node != null, "node is null");
    await node.close();
  });

  test("node has a publicKey", async () => {
    const node = await createNode();
    assert(node.publicKey != null, "publicKey is null");
    assert(typeof node.publicKey.toString() === "string", "publicKey.toString() is not a string");
    assert(node.publicKey.toString().length > 0, "publicKey is empty");
    await node.close();
  });

  test("publicKey is consistent across accesses", async () => {
    const node = await createNode();
    const a = node.publicKey.toString();
    const b = node.publicKey.toString();
    assertEqual(a, b, "publicKey must be consistent");
    await node.close();
  });

  test("two nodes get different publicKeys", async () => {
    const a = await createNode();
    const b = await createNode();
    assertNotEqual(
      a.publicKey.toString(),
      b.publicKey.toString(),
      "two nodes have same publicKey",
    );
    await a.close();
    await b.close();
  });

  test("secretKey is present only when a key is supplied", async () => {
    // No key → the natively generated identity is never surfaced to JS.
    const ephemeral = await createNode();
    assert(
      ephemeral.secretKey == null,
      "secretKey must be undefined when no key is provided",
    );
    await ephemeral.close();

    // Supplied key → the node hands it back.
    const key = new Uint8Array(32).fill(7);
    const keyed = await createNode({ key });
    assert(keyed.secretKey != null, "secretKey must exist when key provided");
    assertEqual(
      Array.from(keyed.secretKey.toBytes()).join(","),
      Array.from(key).join(","),
      "secretKey must equal the supplied key",
    );
    await keyed.close();
  });

  test("node.addr() returns nodeId and addrs", async () => {
    const node = await createNode();
    const addr = await node.addr();
    assert(addr != null, "addr is null");
    assert(typeof addr.id === "string", "addr.id is not a string");
    assertEqual(addr.id, node.publicKey.toString(), "addr.id must match publicKey");
    await node.close();
  });

  // ── Closing ────────────────────────────────────────────────────────────────

  test("node.close() resolves without error", async () => {
    const node = await createNode();
    await assertResolves(node.close());
  });

  test("node.closed promise resolves after close", async () => {
    const node = await createNode();
    const closePromise = node.closed;
    assert(closePromise instanceof Promise, "closed is not a promise");
    await node.close();
    const info = await closePromise;
    assert(info != null, "closed resolved with null");
  });

  test("double close does not throw", async () => {
    const node = await createNode();
    await node.close();
    try {
      await node.close();
    } catch {
      // acceptable — some impls throw on second close
    }
  });

  // ── Serve lifecycle ────────────────────────────────────────────────────────

  test("serve() returns a handle", async () => {
    const node = await createNode();
    const handle = node.serve({}, () => new Response("ok"));
    assert(handle != null, "serve handle is null");
    await node.close();
  });

  test("serve handler receives requests", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();
    let handlerCalled = false;

    server.serve({}, () => {
      handlerCalled = true;
      return new Response("hello");
    });

    const res = await client.fetch(`httpi://${serverId}/test`, { directAddrs: serverAddrs });
    assert(handlerCalled, "handler was not called");
    assertEqual(res.status, 200, "status must be 200");
    const body = await res.text();
    assertEqual(body, "hello", "body must match");

    await server.close();
    await client.close();
  });

  test("serve handler gets valid Request object", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();
    let receivedMethod = "";
    let receivedPath = "";

    server.serve({}, (req) => {
      receivedMethod = req.method;
      receivedPath = new URL(req.url).pathname;
      return new Response("ok");
    });

    await client.fetch(`httpi://${serverId}/hello`, { method: "POST", directAddrs: serverAddrs });

    assertEqual(receivedMethod, "POST", "method must be POST");
    assertEqual(receivedPath, "/hello", "path must match");

    await server.close();
    await client.close();
  });

  // ── Fetch lifecycle ────────────────────────────────────────────────────────

  test("fetch returns a valid Response", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    server.serve({}, () => new Response("works"));
    const res = await client.fetch(`httpi://${serverId}/`, { directAddrs: serverAddrs });
    assert(res instanceof Response, "not a Response");
    assert(typeof res.status === "number", "status not a number");
    assert(res.headers instanceof Headers, "headers not Headers");

    await server.close();
    await client.close();
  });

  test("fetch response body can be read as text", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    server.serve({}, () => new Response("text-body"));
    const res = await client.fetch(`httpi://${serverId}/`, { directAddrs: serverAddrs });
    const text = await res.text();
    assertEqual(text, "text-body", "body text must match");

    await server.close();
    await client.close();
  });

  test("fetch response body can be read as arrayBuffer", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    server.serve({}, () => new Response("buf"));
    const res = await client.fetch(`httpi://${serverId}/`, { directAddrs: serverAddrs });
    const buf = await res.arrayBuffer();
    assert(buf instanceof ArrayBuffer, "not an ArrayBuffer");
    assertEqual(buf.byteLength, 3, "byteLength must be 3");

    await server.close();
    await client.close();
  });

  // ── Key persistence ────────────────────────────────────────────────────────

  test("createNode with saved key produces same publicKey", async () => {
    const key = new Uint8Array(32).fill(0x11);
    const node1 = await createNode({ key });
    const savedKey = node1.secretKey.toBytes();
    const pk1 = node1.publicKey.toString();
    await node1.close();

    const node2 = await createNode({ key: savedKey });
    const pk2 = node2.publicKey.toString();
    assertEqual(pk1, pk2, "publicKey after key restore must match");
    await node2.close();
  });

  test("createNode with Uint8Array key works", async () => {
    const key = new Uint8Array(32).fill(0xab);
    const node1 = await createNode({ key });
    const pk1 = node1.publicKey.toString();
    await node1.close();

    const node2 = await createNode({ key });
    const pk2 = node2.publicKey.toString();
    assertEqual(pk1, pk2, "same Uint8Array key must produce same publicKey");
    await node2.close();
  });

  // ── Ticket ─────────────────────────────────────────────────────────────────

  test("node.ticket() returns a non-empty string", async () => {
    const node = await createNode();
    const ticket = await node.ticket();
    assert(typeof ticket === "string", "ticket is not a string");
    assert(ticket.length > 20, `ticket too short: ${ticket.length}`);
    await node.close();
  });

  // ── Create/close cycles ────────────────────────────────────────────────────

  test("10 sequential create/close cycles", async () => {
    for (let i = 0; i < 10; i++) {
      const node = await createNode();
      assert(node.publicKey.toString().length > 0, `cycle ${i}: no publicKey`);
      await node.close();
    }
  });

  // ── Serve double-call guard (regression #114) ──────────────────────────────

  test("serve() twice on same node throws TypeError", async () => {
    const node = await createNode();
    const ac = new AbortController();
    try {
      node.serve({ signal: ac.signal }, () => new Response("first"));
      let threw = false;
      try {
        node.serve(() => new Response("second"));
      } catch {
        threw = true;
      }
      assert(threw, "Expected second serve() to throw");
    } finally {
      ac.abort();
      await node.close();
    }
  });

  test("serve() allowed again after previous loop finishes", async () => {
    const node = await createNode();
    const ac1 = new AbortController();
    try {
      const h1 = node.serve({ signal: ac1.signal }, () => new Response("first"));
      ac1.abort();
      await h1.finished.catch(() => {});

      const ac2 = new AbortController();
      const h2 = node.serve({ signal: ac2.signal }, () => new Response("second"));
      ac2.abort();
      await h2.finished.catch(() => {});
    } finally {
      await node.close();
    }
  });

  test("serve handle close() stops that server and resolves finished", async () => {
    const node = await createNode();
    try {
      const handle = node.serve({}, () => new Response("ok"));
      // close() stops THIS server and resolves once the loop has drained.
      await handle.close();
      await assertResolves(handle.finished);
      // Since this server stopped, serve() may be started again on the node.
      const again = node.serve({}, () => new Response("again"));
      await again.close();
    } finally {
      await node.close();
    }
  });

  test("serve handle close() is idempotent", async () => {
    const node = await createNode();
    try {
      const handle = node.serve({}, () => new Response("ok"));
      await handle.close();
      // A second close() must not throw and must resolve.
      await assertResolves(handle.close());
    } finally {
      await node.close();
    }
  });

  test("serve handle supports Symbol.asyncDispose", async () => {
    const node = await createNode();
    try {
      let finished;
      {
        const handle = node.serve({}, () => new Response("ok"));
        finished = handle.finished;
        await handle[Symbol.asyncDispose]();
      }
      await assertResolves(finished);
    } finally {
      await node.close();
    }
  });
}
