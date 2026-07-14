import type { IrohAdapter } from "../src/IrohAdapter.ts";
import type { ServiceRecord } from "../src/discovery.ts";
import { advertiseService, browseServices } from "../src/dns-sd.ts";
import { IrohNode } from "../src/IrohNode.ts";

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function assertEquals<T>(actual: T, expected: T, message: string): void {
  if (actual !== expected) {
    throw new Error(
      `${message}: expected ${String(expected)}, got ${String(actual)}`,
    );
  }
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function turn(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

async function waitFor(
  condition: () => boolean,
  message: string,
): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (condition()) return;
    await Promise.resolve();
  }
  throw new Error(message);
}

function record(instanceName: string): ServiceRecord {
  return {
    isActive: true,
    serviceType: "_demo._udp.local.",
    instanceName,
    host: "demo.local.",
    port: 4433,
    addrs: ["192.0.2.1:4433"],
    txt: {},
  };
}

type BrowseAdapter = Pick<IrohAdapter, "browse" | "browseNext" | "browseClose">;
type AdvertiseAdapter = Pick<IrohAdapter, "advertise" | "advertiseClose">;

function iterable(adapter: BrowseAdapter, signal?: AbortSignal) {
  return browseServices(adapter as IrohAdapter, {
    serviceName: "demo",
    signal,
  })[Symbol.asyncIterator]();
}

function advertise(adapter: AdvertiseAdapter, signal?: AbortSignal) {
  return advertiseService(adapter as IrohAdapter, {
    serviceName: "demo",
    instanceName: "instance",
    port: 4433,
    signal,
  });
}

Deno.test("concurrent initial next calls share one native browse", async () => {
  const started = deferred<number>();
  const polls: Array<ReturnType<typeof deferred<ServiceRecord | null>>> = [];
  let starts = 0;
  const adapter: BrowseAdapter = {
    browse() {
      starts += 1;
      return started.promise;
    },
    browseNext() {
      const poll = deferred<ServiceRecord | null>();
      polls.push(poll);
      return poll.promise;
    },
    browseClose() {},
  };
  const iterator = iterable(adapter);

  const first = iterator.next();
  const second = iterator.next();
  await turn();
  assertEquals(starts, 1, "only one native browse may start");

  started.resolve(17);
  await waitFor(() => polls.length === 1, "first poll did not start");
  assertEquals(polls.length, 1, "concurrent polls must be serialized");
  polls[0].resolve(record("first"));
  const firstResult = await first;
  assert(!firstResult.done, "first record should be yielded");

  await waitFor(() => polls.length === 2, "second poll did not start");
  assertEquals(polls.length, 2, "second poll should run after the first");
  polls[1].resolve(record("second"));
  const secondResult = await second;
  assert(!secondResult.done, "second record should be yielded");
  assertEquals(starts, 1, "the shared native browse must not restart");
});

Deno.test("terminal null is sticky and closes exactly once", async () => {
  let starts = 0;
  let polls = 0;
  const closed: number[] = [];
  const adapter: BrowseAdapter = {
    browse() {
      starts += 1;
      return Promise.resolve(23);
    },
    browseNext() {
      polls += 1;
      return Promise.resolve(null);
    },
    browseClose(handle) {
      closed.push(handle);
    },
  };
  const iterator = iterable(adapter);

  assert(
    (await iterator.next()).done,
    "terminal poll should finish the iterator",
  );
  assert((await iterator.next()).done, "later next calls must remain finished");
  assert(
    (await iterator.return!()).done,
    "return after terminal must remain finished",
  );
  assertEquals(starts, 1, "a finished iterator must not restart");
  assertEquals(polls, 1, "a finished iterator must not poll a stale handle");
  assertEquals(closed.length, 1, "the native browse must close once");
  assertEquals(closed[0], 23, "the active handle should be closed");
});

Deno.test("return during start closes the late handle without polling", async () => {
  const started = deferred<number>();
  const closed: number[] = [];
  let polls = 0;
  const adapter: BrowseAdapter = {
    browse: () => started.promise,
    browseNext() {
      polls += 1;
      return Promise.resolve(record("unexpected"));
    },
    browseClose(handle) {
      closed.push(handle);
    },
  };
  const iterator = iterable(adapter);

  const pending = iterator.next();
  await turn();
  assert(
    (await iterator.return!()).done,
    "return should finish while start is pending",
  );
  assert((await pending).done, "the pending next should be released by return");

  started.resolve(31);
  await turn();
  assertEquals(closed.length, 1, "a handle that arrives late must be closed");
  assertEquals(closed[0], 31, "the late handle should be retired");
  assertEquals(polls, 0, "a finished iterator must not poll a late handle");
  assert(
    (await iterator.next()).done,
    "next after return must remain finished",
  );
});

Deno.test("sticky next does not await detached late-start cleanup", async () => {
  const started = deferred<number>();
  const close = deferred<void>();
  let closes = 0;
  const adapter: BrowseAdapter = {
    browse: () => started.promise,
    browseNext: () => Promise.resolve(record("unexpected")),
    browseClose() {
      closes += 1;
      return close.promise;
    },
  };
  const iterator = iterable(adapter);

  const pending = iterator.next();
  await turn();
  assert((await iterator.return!()).done, "return should settle before start");
  assert((await pending).done, "pending next should settle before start");
  started.resolve(37);
  await waitFor(() => closes === 1, "late handle cleanup was not dispatched");

  assert(
    (await iterator.next()).done,
    "sticky next must not wait for detached late cleanup",
  );
  close.resolve();
  await turn();
  assertEquals(closes, 1, "late native handle must close exactly once");
});

class CountingAbortSignal extends EventTarget {
  aborted = false;
  added = 0;
  removed = 0;

  override addEventListener(
    type: string,
    callback: EventListenerOrEventListenerObject | null,
    options?: AddEventListenerOptions | boolean,
  ): void {
    if (type === "abort") this.added += 1;
    super.addEventListener(type, callback, options);
  }

  override removeEventListener(
    type: string,
    callback: EventListenerOrEventListenerObject | null,
    options?: EventListenerOptions | boolean,
  ): void {
    if (type === "abort") this.removed += 1;
    super.removeEventListener(type, callback, options);
  }

  abort(): void {
    if (this.aborted) return;
    this.aborted = true;
    this.dispatchEvent(new Event("abort"));
  }
}

Deno.test("one abort listener covers the session and is removed on finish", async () => {
  const signal = new CountingAbortSignal();
  const polls: Array<ReturnType<typeof deferred<ServiceRecord | null>>> = [];
  const closed: number[] = [];
  const adapter: BrowseAdapter = {
    browse: () => Promise.resolve(41),
    browseNext() {
      const poll = deferred<ServiceRecord | null>();
      polls.push(poll);
      return poll.promise;
    },
    browseClose(handle) {
      closed.push(handle);
    },
  };
  const iterator = iterable(adapter, signal as unknown as AbortSignal);

  const first = iterator.next();
  await waitFor(() => polls.length === 1, "first poll did not start");
  polls[0].resolve(record("first"));
  assert(!(await first).done, "first record should be yielded");

  const second = iterator.next();
  await waitFor(() => polls.length === 2, "second poll did not start");
  polls[1].resolve(record("second"));
  assert(!(await second).done, "second record should be yielded");
  assertEquals(signal.added, 1, "polls must share one abort listener");

  const pending = iterator.next();
  await waitFor(() => polls.length === 3, "terminal poll did not start");
  signal.abort();
  assert((await pending).done, "abort should release the pending poll");
  assertEquals(signal.removed, 1, "terminal cleanup must remove the listener");
  assertEquals(closed.length, 1, "abort must close the native browse once");
  assert((await iterator.next()).done, "next after abort must remain finished");
});

Deno.test("an iterator abandoned before next installs no abort listener", async () => {
  const signal = new CountingAbortSignal();
  let starts = 0;
  const adapter: BrowseAdapter = {
    browse() {
      starts += 1;
      return Promise.resolve(43);
    },
    browseNext: () => Promise.resolve(null),
    browseClose() {},
  };

  const iterator = iterable(adapter, signal as unknown as AbortSignal);
  assertEquals(
    signal.added,
    0,
    "creating an iterator must not retain its signal",
  );
  assert((await iterator.return!()).done, "unused iterator should return done");
  assertEquals(signal.added, 0, "return before next must not add a listener");
  assertEquals(starts, 0, "return before next must not start browsing");
});

Deno.test("return waits for an active asynchronous native close", async () => {
  const poll = deferred<ServiceRecord | null>();
  const close = deferred<void>();
  let polls = 0;
  let closes = 0;
  const adapter: BrowseAdapter = {
    browse: () => Promise.resolve(47),
    browseNext() {
      polls += 1;
      return poll.promise;
    },
    browseClose() {
      closes += 1;
      return close.promise;
    },
  };
  const iterator = iterable(adapter);

  const pending = iterator.next();
  await waitFor(() => polls === 1, "native poll did not start");
  let returned = false;
  const stopping = iterator.return!().then((result) => {
    returned = true;
    return result;
  });
  await turn();
  assertEquals(closes, 1, "return should dispatch one native close");
  assert(!returned, "return must remain pending until native close completes");

  close.resolve();
  assert((await stopping).done, "return should finish after native close");
  assert((await pending).done, "native close should release the pending poll");
  assertEquals(closes, 1, "native close must remain exactly once");
});

Deno.test("next after abort between records awaits asynchronous close", async () => {
  const controller = new AbortController();
  const close = deferred<void>();
  let polls = 0;
  let closes = 0;
  const adapter: BrowseAdapter = {
    browse: () => Promise.resolve(48),
    browseNext() {
      polls += 1;
      return Promise.resolve(record("first"));
    },
    browseClose() {
      closes += 1;
      return close.promise;
    },
  };
  const iterator = iterable(adapter, controller.signal);

  assert(!(await iterator.next()).done, "the first record should be yielded");
  controller.abort();
  await waitFor(() => closes === 1, "abort did not dispatch native close");

  let settled = false;
  const terminal = iterator.next().then((result) => {
    settled = true;
    return result;
  });
  await turn();
  assert(!settled, "terminal next must await the native close barrier");
  assertEquals(polls, 1, "abort between records must not start another poll");

  close.resolve();
  assert((await terminal).done, "next should finish after native close");
  assertEquals(closes, 1, "native close must remain exactly once");
});

Deno.test("poll failure remains primary when cleanup also fails", async () => {
  const pollFailure = new Error("poll failed");
  const cleanupFailure = new Error("close failed");
  const adapter: BrowseAdapter = {
    browse: () => Promise.resolve(49),
    browseNext: () => Promise.reject(pollFailure),
    browseClose() {
      throw cleanupFailure;
    },
  };
  const iterator = iterable(adapter);

  let observed: unknown;
  try {
    await iterator.next();
  } catch (error) {
    observed = error;
  }
  assertEquals(observed, pollFailure, "cleanup must not mask the poll failure");
  assert(
    (await iterator.next()).done,
    "a failed iterator must remain terminal",
  );
  assert(
    (await iterator.return!()).done,
    "consumed cleanup failure must not break terminal return",
  );
});

Deno.test("return reports a native close failure only once", async () => {
  const cleanupFailure = new Error("close failed");
  const adapter: BrowseAdapter = {
    browse: () => Promise.resolve(51),
    browseNext: () => Promise.resolve(record("first")),
    browseClose() {
      throw cleanupFailure;
    },
  };
  const iterator = iterable(adapter);
  assert(!(await iterator.next()).done, "the first record should be yielded");

  let observed: unknown;
  try {
    await iterator.return!();
  } catch (error) {
    observed = error;
  }
  assertEquals(observed, cleanupFailure, "first return should report close");
  assert(
    (await iterator.return!()).done,
    "second return should retain sticky termination",
  );
  assert((await iterator.next()).done, "next after return must remain done");
});

Deno.test("late cleanup failure is observed instead of becoming unhandled", async () => {
  const started = deferred<number>();
  const cleanupFailure = new Error("late close failed");
  const reported: unknown[][] = [];
  const originalError = console.error;
  console.error = (...args: unknown[]) => reported.push(args);
  try {
    const adapter: BrowseAdapter = {
      browse: () => started.promise,
      browseNext: () => Promise.resolve(null),
      browseClose() {
        throw cleanupFailure;
      },
    };
    const iterator = iterable(adapter);
    const pending = iterator.next();
    await turn();
    assert(
      (await iterator.return!()).done,
      "return should settle before readiness",
    );
    assert((await pending).done, "pending next should settle before readiness");

    started.resolve(53);
    await waitFor(
      () => reported.length === 1,
      "late cleanup failure was not observed",
    );
    assertEquals(
      reported[0][1],
      cleanupFailure,
      "cleanup report should retain the failure",
    );
    assert(
      (await iterator.next()).done,
      "detached cleanup failure must not break sticky termination",
    );
  } finally {
    console.error = originalError;
  }
});

Deno.test("peer browse uses the shared lifecycle and skips self records", async () => {
  const selfId = "a".repeat(52);
  const peerId = "b".repeat(52);
  const events = [
    { type: "discovered" as const, nodeId: selfId, addrs: ["192.0.2.1:1"] },
    { type: "expired" as const, nodeId: peerId },
  ];
  let starts = 0;
  let polls = 0;
  const closed: number[] = [];
  const adapter = {
    startTransportEvents() {},
    browsePeers() {
      starts += 1;
      return Promise.resolve(59);
    },
    browsePeersNext() {
      polls += 1;
      return Promise.resolve(events.shift() ?? null);
    },
    browsePeersClose(handle: number) {
      closed.push(handle);
    },
  } as unknown as IrohAdapter;
  const node = IrohNode._create(
    adapter,
    { endpointHandle: 1, nodeId: selfId },
    undefined,
    new Promise<void>(() => {}),
  );
  const iterator = node.browsePeers()[Symbol.asyncIterator]();

  const result = await iterator.next();
  assert(!result.done, "the non-self peer should be yielded");
  assertEquals(result.value.nodeId, peerId, "the self record must be skipped");
  assertEquals(
    result.value.isActive,
    false,
    "expired event should remain inactive",
  );
  assertEquals(
    result.value.addrs.length,
    0,
    "missing expiry addresses normalize empty",
  );
  assertEquals(starts, 1, "peer browse should start once");
  assertEquals(polls, 2, "self skipping should continue the same native poll");
  assert((await iterator.return!()).done, "peer iterator should close cleanly");
  assertEquals(closed.length, 1, "peer handle should close exactly once");
  assertEquals(closed[0], 59, "the peer browse handle should be retired");
});

Deno.test("pre-aborted advertisement does not start natively", async () => {
  const controller = new AbortController();
  controller.abort();
  let starts = 0;
  let closes = 0;
  const adapter: AdvertiseAdapter = {
    advertise() {
      starts += 1;
      return Promise.resolve(51);
    },
    advertiseClose() {
      closes += 1;
    },
  };

  await advertise(adapter, controller.signal);
  assertEquals(starts, 0, "pre-aborted advertising must not start");
  assertEquals(closes, 0, "no nonexistent advertisement should be closed");
});

Deno.test("abort settles a pending advertisement and closes its late handle once", async () => {
  const controller = new AbortController();
  const started = deferred<number>();
  const closed: number[] = [];
  let starts = 0;
  const adapter: AdvertiseAdapter = {
    advertise() {
      starts += 1;
      return started.promise;
    },
    advertiseClose(handle) {
      closed.push(handle);
    },
  };

  const advertising = advertise(adapter, controller.signal);
  await waitFor(() => starts === 1, "native advertisement did not start");
  controller.abort();
  let timer: number | undefined;
  const outcome = await Promise.race([
    advertising.then(() => "settled" as const),
    new Promise<"timeout">((resolve) => {
      timer = setTimeout(() => resolve("timeout"), 100);
    }),
  ]);
  if (timer !== undefined) clearTimeout(timer);
  assertEquals(outcome, "settled", "abort must settle before native readiness");
  assertEquals(
    closed.length,
    0,
    "there is no handle to close before readiness",
  );

  started.resolve(61);
  await turn();
  assertEquals(closed.length, 1, "the late handle must close exactly once");
  assertEquals(
    closed[0],
    61,
    "the late advertisement handle should be retired",
  );
  controller.abort();
  await turn();
  assertEquals(closed.length, 1, "repeated abort must not close twice");
});

Deno.test("active advertisement abort waits for asynchronous native close", async () => {
  const controller = new AbortController();
  const close = deferred<void>();
  let starts = 0;
  let closes = 0;
  const adapter: AdvertiseAdapter = {
    advertise() {
      starts += 1;
      return Promise.resolve(63);
    },
    advertiseClose() {
      closes += 1;
      return close.promise;
    },
  };

  let settled = false;
  const advertising = advertise(adapter, controller.signal).then(() => {
    settled = true;
  });
  await waitFor(() => starts === 1, "native advertisement did not start");
  await turn();
  controller.abort();
  await waitFor(
    () => closes === 1,
    "native advertisement close was not dispatched",
  );
  assert(!settled, "advertisement must await the native unregister barrier");

  close.resolve();
  await advertising;
  assert(settled, "advertisement should settle after native unregister");
  assertEquals(closes, 1, "native advertisement should close exactly once");
});

Deno.test("advertisement start rejection removes its abort listener", async () => {
  const signal = new CountingAbortSignal();
  const failure = new Error("registration failed");
  const closed: number[] = [];
  const adapter: AdvertiseAdapter = {
    advertise: () => Promise.reject(failure),
    advertiseClose(handle) {
      closed.push(handle);
    },
  };

  let observed: unknown;
  try {
    await advertise(adapter, signal as unknown as AbortSignal);
  } catch (error) {
    observed = error;
  }
  assertEquals(observed, failure, "the start failure should reach the caller");
  assertEquals(signal.added, 1, "start should install one abort listener");
  assertEquals(signal.removed, 1, "start failure must remove the listener");
  signal.abort();
  await turn();
  assertEquals(closed.length, 0, "abort after start failure must be inert");
});
