/**
 * Discovery tests — browsePeers(), pathChanges(), advertisePeer() API surface.
 *
 * These test the API shape (returns AsyncIterable / Promise), not actual mDNS
 * discovery which requires multicast UDP and is unreliable in CI.
 *
 * Shared across all runtimes.
 */

export function discoveryTests({ createNode, test, assert }) {
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
}
