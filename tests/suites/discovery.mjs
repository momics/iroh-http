/**
 * Discovery tests — browsePeers(), pathChanges(), advertisePeer() API surface,
 * plus the generic node.advertise()/node.browse()/asIrohPeer() surface (#330
 * review nit: previously only the peer specialization had coverage here).
 *
 * These test the API shape (returns AsyncIterable / Promise), not actual mDNS
 * discovery which requires multicast UDP and is unreliable in CI.
 *
 * Shared across all runtimes.
 */

export function discoveryTests({ createNode, test, assert, asIrohPeer }) {
  test("browsePeers() returns an AsyncIterable", async () => {
    const node = await createNode({ disableNetworking: true });
    try {
      const iterable = node.browsePeers();
      assert(
        typeof iterable[Symbol.asyncIterator] === "function",
        "browsePeers() must return an AsyncIterable",
      );
    } finally {
      await node.close();
    }
  });

  test("pathChanges() returns an AsyncIterable", async () => {
    const node = await createNode({ disableNetworking: true });
    try {
      const iterable = node.pathChanges(node.publicKey);
      assert(
        typeof iterable[Symbol.asyncIterator] === "function",
        "pathChanges() must return an AsyncIterable",
      );
    } finally {
      await node.close();
    }
  });

  // Parity guard: every adapter must implement the path-change subscription
  // FFI (nextPathChange + unsubscribePathChanges). Before the Tauri adapter
  // overrode these, this iteration rejected with "nextPathChange() not
  // supported by this adapter". Aborting mid-subscription must wake the
  // pending nextPathChange (via unsubscribePathChanges) and complete cleanly.
  test("pathChanges() subscription resolves when aborted", async () => {
    const node = await createNode({ disableNetworking: true });
    try {
      // Aborting before the first poll must not create a native cancellation
      // marker that affects a later iterator for the same peer.
      const beforePoll = new AbortController();
      const idleIterator = node
        .pathChanges(node.publicKey, { signal: beforePoll.signal })
        [Symbol.asyncIterator]();
      beforePoll.abort();
      assert(
        (await idleIterator.next()).done === true,
        "pathChanges() aborted before polling must complete",
      );

      // Repeat on the same endpoint/peer to prove a pre-registration
      // cancellation is consumed rather than poisoning the next subscription.
      for (let cycle = 0; cycle < 10; cycle++) {
        const ac = new AbortController();
        const iterator = node
          .pathChanges(node.publicKey, { signal: ac.signal })
          [Symbol.asyncIterator]();
        const pending = iterator.next();
        let settled = false;
        void pending.then(() => settled = true, () => settled = true);
        await new Promise((resolve) => setTimeout(resolve, 5));
        assert(
          !settled,
          `pathChanges() must remain live before abort (cycle ${cycle})`,
        );
        ac.abort();
        const result = await pending;
        assert(
          result.done === true,
          `aborted pathChanges() iterator must complete (cycle ${cycle})`,
        );
      }

      // Cancellation ownership is per iterator. Stopping one overlapping poll
      // must not remove the other iterator's core subscription.
      for (let cycle = 0; cycle < 10; cycle++) {
        const firstAbort = new AbortController();
        const secondAbort = new AbortController();
        const first = node
          .pathChanges(node.publicKey, { signal: firstAbort.signal })
          [Symbol.asyncIterator]().next();
        const second = node
          .pathChanges(node.publicKey, { signal: secondAbort.signal })
          [Symbol.asyncIterator]().next();
        firstAbort.abort();
        assert(
          (await first).done === true,
          `first overlapping iterator must complete (cycle ${cycle})`,
        );
        let secondSettled = false;
        void second.then(
          () => secondSettled = true,
          () => secondSettled = true,
        );
        await new Promise((resolve) => setTimeout(resolve, 5));
        assert(
          !secondSettled,
          `second iterator must remain live after first abort (cycle ${cycle})`,
        );
        secondAbort.abort();
        assert(
          (await second).done === true,
          `second overlapping iterator must complete (cycle ${cycle})`,
        );
      }
    } finally {
      await node.close();
    }
  });

  test("advertisePeer() resolves when signal is aborted", async () => {
    const node = await createNode();
    try {
      const ac = new AbortController();
      const p = node.advertisePeer({ signal: ac.signal });
      ac.abort();
      await p;
    } finally {
      await node.close();
    }
  });

  // ── Generic DNS-SD surface ──────────────────────────────────────────────
  //
  // Unlike the iroh-peer specialization above, `advertise()`/`browse()` take
  // full `ServiceConfig`/`DnsSdBrowseOptions` — arbitrary services, not just
  // iroh nodes.

  test("browse() returns an AsyncIterable", async () => {
    const node = await createNode({ disableNetworking: true });
    try {
      const iterable = node.browse({ serviceName: "iroh-http-test" });
      assert(
        typeof iterable[Symbol.asyncIterator] === "function",
        "browse() must return an AsyncIterable",
      );
    } finally {
      await node.close();
    }
  });

  test("advertise() resolves when signal is aborted", async () => {
    const node = await createNode();
    try {
      const ac = new AbortController();
      const p = node.advertise({
        serviceName: "iroh-http-test",
        instanceName: "discovery-test-instance",
        port: 8080,
        signal: ac.signal,
      });
      ac.abort();
      await p;
    } finally {
      await node.close();
    }
  });

  test("asIrohPeer() reads the pk TXT property as nodeId", () => {
    const record = {
      isActive: true,
      serviceType: "_iroh-http._udp.local.",
      instanceName: "some-instance",
      host: "example.local.",
      port: 1234,
      addrs: ["127.0.0.1:1234"],
      txt: { pk: "abcdef234567" },
    };
    const peer = asIrohPeer(record);
    assert(peer !== null, "asIrohPeer() must recognize a record with a pk TXT");
    assert(peer.nodeId === "abcdef234567", "nodeId must come from the pk TXT");
    assert(peer.isActive === true, "isActive must be carried through");
    assert(
      Array.isArray(peer.addrs) && peer.addrs.length === 1,
      "addrs must be carried through",
    );
  });

  test("asIrohPeer() falls back to the instance name without a pk TXT", () => {
    const record = {
      isActive: true,
      serviceType: "_iroh-http._udp.local.",
      instanceName: "fallback-instance",
      host: "example.local.",
      port: 1234,
      addrs: [],
      txt: {},
    };
    const peer = asIrohPeer(record);
    assert(
      peer !== null && peer.nodeId === "fallback-instance",
      "asIrohPeer() must fall back to the instance name",
    );
  });
}
