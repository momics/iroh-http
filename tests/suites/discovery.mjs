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
      const ac = new AbortController();
      const iterator = node
        .pathChanges(node.publicKey, { signal: ac.signal })
        [Symbol.asyncIterator]();
      const pending = iterator.next();
      ac.abort();
      const result = await pending;
      assert(
        result.done === true,
        "aborted pathChanges() iterator must complete",
      );
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
