/**
 * Stress tests — high concurrency, large payloads, rapid lifecycle churn,
 * resource behavior under load.
 *
 * Shared across all runtimes.
 */

export function stressTests({ createNode, test, assert, assertEqual }) {
  // ── Concurrent fetch ───────────────────────────────────────────────────────

  test("50 concurrent GETs all succeed", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    server.serve({}, () => new Response("ok"));

    const results = await Promise.all(
      Array.from({ length: 50 }, (_, i) =>
        client.fetch(`httpi://${serverId}/concurrent-${i}`, { directAddrs: serverAddrs }).then(async (res) => {
          const body = await res.text();
          return { status: res.status, body };
        })
      )
    );

    for (let i = 0; i < results.length; i++) {
      assertEqual(results[i].status, 200, `request ${i} status`);
      assertEqual(results[i].body, "ok", `request ${i} body`);
    }

    await server.close();
    await client.close();
  });

  test("20 concurrent POSTs with body all echo correctly", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    server.serve({}, async (req) => {
      const body = await req.text();
      return new Response(body);
    });

    const results = await Promise.all(
      Array.from({ length: 20 }, (_, i) =>
        client.fetch(`httpi://${serverId}/echo`, {
          method: "POST",
          body: `message-${i}`,
          directAddrs: serverAddrs,
        }).then(async (res) => {
          const body = await res.text();
          return { status: res.status, body, index: i };
        })
      )
    );

    for (const r of results) {
      assertEqual(r.status, 200, `POST ${r.index} status`);
      assertEqual(r.body, `message-${r.index}`, `POST ${r.index} body`);
    }

    await server.close();
    await client.close();
  });

  // ── Sequential fetch (connection reuse) ────────────────────────────────────

  test("100 sequential GETs all succeed", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    let count = 0;
    server.serve({}, () => {
      count++;
      return new Response(`req-${count}`);
    });

    for (let i = 0; i < 100; i++) {
      const res = await client.fetch(`httpi://${serverId}/seq-${i}`, { directAddrs: serverAddrs });
      assertEqual(res.status, 200, `seq ${i} status`);
      const body = await res.text();
      assert(body.startsWith("req-"), `seq ${i} body prefix`);
    }

    assertEqual(count, 100, "handler call count");

    await server.close();
    await client.close();
  });

  // ── Large body transfers ───────────────────────────────────────────────────

  test("1MB body round-trip", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    server.serve({}, async (req) => {
      const buf = await req.arrayBuffer();
      return new Response(new Uint8Array(buf));
    });

    const MB = 1024 * 1024;
    const payload = new Uint8Array(MB);
    for (let i = 0; i < MB; i++) payload[i] = i & 0xff;

    const res = await client.fetch(`httpi://${serverId}/echo`, {
      method: "POST",
      body: payload,
      directAddrs: serverAddrs,
    });

    assertEqual(res.status, 200, "status");
    const buf = await res.arrayBuffer();
    assertEqual(buf.byteLength, MB, "response size");

    const view = new Uint8Array(buf);
    assertEqual(view[0], 0, "first byte");
    assertEqual(view[1], 1, "second byte");
    assertEqual(view[MB - 1], (MB - 1) & 0xff, "last byte");

    await server.close();
    await client.close();
  });

  test("5 concurrent 256KB transfers", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    server.serve({}, async (req) => {
      const buf = await req.arrayBuffer();
      return new Response(String(buf.byteLength));
    });

    const SIZE = 256 * 1024;
    const results = await Promise.all(
      Array.from({ length: 5 }, () =>
        client.fetch(`httpi://${serverId}/echo-length`, {
          method: "POST",
          body: new Uint8Array(SIZE),
          directAddrs: serverAddrs,
        }).then(async (res) => {
          const body = await res.text();
          return { status: res.status, bodyLength: parseInt(body, 10) };
        })
      )
    );

    for (let i = 0; i < results.length; i++) {
      assertEqual(results[i].status, 200, `transfer ${i} status`);
      assertEqual(results[i].bodyLength, SIZE, `transfer ${i} size`);
    }

    await server.close();
    await client.close();
  });

  // ── Rapid lifecycle churn ──────────────────────────────────────────────────

  test("20 rapid create/close cycles with fetch", async () => {
    for (let i = 0; i < 20; i++) {
      const server = await createNode();
      const client = await createNode();
      const { id: serverId, addrs: serverAddrs } = await server.addr();

      server.serve({}, () => new Response(`cycle-${i}`));

      const res = await client.fetch(`httpi://${serverId}/`, { directAddrs: serverAddrs });
      assertEqual(res.status, 200, `cycle ${i} status`);
      const body = await res.text();
      assertEqual(body, `cycle-${i}`, `cycle ${i} body`);

      await server.close();
      await client.close();
    }
  });

  // ── Multiple concurrent nodes ──────────────────────────────────────────────

  test("5 independent node pairs concurrently", async () => {
    const pairs = await Promise.all(
      Array.from({ length: 5 }, async (_, i) => {
        const server = await createNode();
        const client = await createNode();
        const { id: serverId, addrs: serverAddrs } = await server.addr();
        server.serve({}, () => new Response(`pair-${i}`));
        return { server, client, serverId, serverAddrs, index: i };
      })
    );

    const results = await Promise.all(
      pairs.map(async ({ client, serverId, serverAddrs, index }) => {
        const res = await client.fetch(`httpi://${serverId}/`, { directAddrs: serverAddrs });
        const body = await res.text();
        return { index, status: res.status, body };
      })
    );

    for (const r of results) {
      assertEqual(r.status, 200, `pair ${r.index} status`);
      assertEqual(r.body, `pair-${r.index}`, `pair ${r.index} body`);
    }

    await Promise.all(
      pairs.flatMap(({ server, client }) => [server.close(), client.close()])
    );
  });

  // ── Mixed methods under load ───────────────────────────────────────────────

  test("30 concurrent mixed-method requests", async () => {
    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    server.serve({}, (req) => new Response(req.method));

    const methods = ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
    const results = await Promise.all(
      Array.from({ length: 30 }, (_, i) => {
        const method = methods[i % methods.length];
        return client.fetch(`httpi://${serverId}/method-${i}`, { method, directAddrs: serverAddrs }).then(async (res) => {
          const body = await res.text();
          return { method, body, status: res.status };
        });
      })
    );

    for (const r of results) {
      assertEqual(r.status, 200, `${r.method} status`);
      assertEqual(r.body, r.method, `${r.method} echo`);
    }

    await server.close();
    await client.close();
  });

  // ── Regression: stale-handle errors after serve restart (#119) ─────────────

  test("32-stream burst × 5 iterations: no stale-handle errors (regression #119)", async () => {
    const STREAMS = 32;
    const ITERS = 5;
    const BODY = "x".repeat(4096);

    const errors = [];
    const originalError = console.error.bind(console);
    console.error = (...args) => {
      const msg = args.map(String).join(" ");
      if (msg.includes("unknown handle") || msg.includes("node closed or not found") || msg.includes("sendChunk failed")) {
        errors.push(msg);
      }
      originalError(...args);
    };

    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    try {
      for (let iter = 0; iter < ITERS; iter++) {
        const ac = new AbortController();
        const handle = server.serve({ signal: ac.signal, loadShed: false }, () => new Response(BODY));

        await Promise.all(
          Array.from({ length: STREAMS }, () =>
            client.fetch(`httpi://${serverId}/data`, { directAddrs: serverAddrs }).then((r) => r.text())
          ),
        );

        ac.abort();
        await handle.finished;
      }

      assertEqual(errors.length, 0, `stale-handle errors: ${errors.join(" | ")}`);
    } finally {
      console.error = originalError;
      await server.close();
      await client.close();
    }
  });

  // ── Regression: dispatch-level 503 during serve stop/restart (#149) ────────

  test("no dispatch-level 503 during stop/restart cycles (regression #149)", async () => {
    const STREAMS = 32;
    const ITERS = 10;
    const BODY = "x".repeat(4096);

    const server = await createNode();
    const client = await createNode();
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    try {
      for (let iter = 0; iter < ITERS; iter++) {
        const ac = new AbortController();
        const handle = server.serve({ signal: ac.signal, loadShed: false }, () => new Response(BODY));

        const results = await Promise.all(
          Array.from({ length: STREAMS }, () =>
            client.fetch(`httpi://${serverId}/data`, { directAddrs: serverAddrs }).then(async (r) => ({ status: r.status }))
          ),
        );

        ac.abort();
        await handle.finished;

        for (let i = 0; i < STREAMS; i++) {
          assertEqual(results[i].status, 200, `iter ${iter} stream ${i}: expected 200, got ${results[i].status}`);
        }
      }
    } finally {
      await server.close();
      await client.close();
    }
  });
}
