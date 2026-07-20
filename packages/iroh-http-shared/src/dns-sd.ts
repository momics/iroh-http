import type { IrohAdapter } from "./IrohAdapter.js";
import type {
  DnsSdAdvertiseOptions,
  DnsSdBrowseOptions,
  ServiceConfig,
  ServiceRecord,
} from "./discovery.js";

interface DiscoveryIteratorOptions<Input, Output> {
  signal?: AbortSignal;
  start(): Promise<number>;
  poll(handle: number): Promise<Input | null>;
  close(handle: number): void | Promise<void>;
  map(value: Input): Output | undefined;
}

interface DiscoveryAdvertisementOptions {
  signal?: AbortSignal;
  start(): Promise<number>;
  close(handle: number): void | Promise<void>;
}

function cleanup(
  close: (handle: number) => void | Promise<void>,
  handle: number,
): Promise<void> {
  return Promise.resolve().then(() => close(handle));
}

function observeDetachedCleanup(
  promise: Promise<void>,
  operation: string,
): void {
  void promise.catch((error) => {
    // A return/abort that wins while native start is pending must settle
    // promptly, so a later handle has no caller left to receive cleanup errors.
    // Observe and report that failure rather than creating an unhandled promise.
    console.error(
      `[iroh-http] ${operation} cleanup failed after cancellation`,
      error,
    );
  });
}

/**
 * Build one linearized discovery iterator around a native start/poll/close
 * lifecycle.
 *
 * This is shared by peer and generic DNS-SD browsing so concurrent `next()`
 * calls cannot race two starts, and termination has the same sticky semantics
 * on every adapter. The single abort listener also avoids retaining one
 * listener per yielded record for the lifetime of a long-running browse.
 *
 * @internal
 */
export function createDiscoveryIterator<Input, Output>(
  options: DiscoveryIteratorOptions<Input, Output>,
): AsyncIterable<Output> {
  const { signal, start, poll, close, map } = options;

  return {
    [Symbol.asyncIterator]() {
      let finished = false;
      let startPromise: Promise<number> | null = null;
      let handle: number | null = null;
      let closePromise: Promise<void> | null = null;
      let closeFailureConsumed = false;
      let abortListenerRegistered = false;
      let sequence: Promise<void> = Promise.resolve();
      let resolveFinished!: () => void;
      const finishedSignal = new Promise<void>((resolve) => {
        resolveFinished = resolve;
      });

      const done = (): IteratorResult<Output> => ({
        done: true,
        value: undefined,
      });

      const closeHandle = (value: number): Promise<void> => {
        if (closePromise !== null) return closePromise;
        if (handle === value) handle = null;
        closePromise = cleanup(close, value);
        return closePromise;
      };

      const finish = (): Promise<void> => {
        if (!finished) {
          finished = true;
          if (signal && abortListenerRegistered) {
            signal.removeEventListener("abort", onAbort);
            abortListenerRegistered = false;
          }
          // Wake a pending start/poll before awaiting the adapter's close hook.
          // A failed close must not strand another queued `next()`.
          resolveFinished();
        }
        if (handle !== null) return closeHandle(handle);
        return closePromise ?? Promise.resolve();
      };

      const onAbort = (): void => {
        observeDetachedCleanup(finish(), "discovery browse");
      };

      const observeSignal = (): boolean => {
        if (finished) return false;
        if (!signal) return true;
        if (signal.aborted) {
          // No native start has occurred yet, so this resolves synchronously.
          void finish();
          return false;
        }
        if (!abortListenerRegistered) {
          signal.addEventListener("abort", onAbort, { once: true });
          abortListenerRegistered = true;
        }
        return true;
      };

      const ensureHandle = async (): Promise<number | null> => {
        if (finished) return null;
        if (!observeSignal()) return null;
        if (handle !== null) return handle;
        if (startPromise === null) {
          // Capture a synchronous adapter throw as a rejected promise and keep
          // exactly one start in flight for all queued `next()` calls.
          startPromise = Promise.resolve().then(start);
          void startPromise.then(
            (startedHandle) => {
              if (finished) {
                observeDetachedCleanup(
                  // No caller remains to await this handle, so keep its
                  // best-effort retirement separate from the iterator's
                  // terminal close promise. A later sticky `next()` must not
                  // re-surface a detached cleanup failure that was already
                  // reported here.
                  cleanup(close, startedHandle),
                  "late discovery browse",
                );
              } else {
                handle = startedHandle;
              }
            },
            () => {
              // `ensureHandle` propagates this rejection to the active next().
            },
          );
        }

        const outcome = await Promise.race([
          startPromise.then((startedHandle) => ({
            kind: "started" as const,
            handle: startedHandle,
          })),
          finishedSignal.then(() => ({ kind: "finished" as const })),
        ]);
        if (outcome.kind === "finished" || finished) return null;
        handle = outcome.handle;
        return outcome.handle;
      };

      const nextValue = async (): Promise<IteratorResult<Output>> => {
        try {
          const activeHandle = await ensureHandle();
          if (activeHandle === null) {
            // Abort may have started an asynchronous native close between two
            // yielded records. Preserve the close barrier even though there is
            // no poll in flight to await it on the caller's behalf.
            try {
              await finish();
            } catch (error) {
              if (!closeFailureConsumed) throw error;
            }
            return done();
          }

          while (!finished) {
            const outcome = await Promise.race([
              poll(activeHandle).then((value) => ({
                kind: "value" as const,
                value,
              })),
              finishedSignal.then(() => ({ kind: "finished" as const })),
            ]);
            if (outcome.kind === "finished" || finished) {
              await finish();
              return done();
            }
            if (outcome.value === null) {
              await finish();
              return done();
            }
            const mapped = map(outcome.value);
            if (mapped !== undefined) {
              return { done: false, value: mapped };
            }
          }
          return done();
        } catch (error) {
          try {
            await finish();
          } catch {
            closeFailureConsumed = true;
            // Preserve the start/poll/map failure as the primary error. The
            // iterator is already terminal and the cleanup rejection is fully
            // observed here, so it cannot escape as an unhandled promise.
          }
          throw error;
        }
      };

      return {
        next(): Promise<IteratorResult<Output>> {
          // Match async-generator behavior: concurrent next calls are queued,
          // not allowed to start/poll independent native sessions.
          const result = sequence.then(nextValue);
          sequence = result.then(
            () => undefined,
            () => undefined,
          );
          return result;
        },
        async return(): Promise<IteratorResult<Output>> {
          try {
            await finish();
          } catch (error) {
            if (closeFailureConsumed) return done();
            closeFailureConsumed = true;
            throw error;
          }
          return done();
        },
      };
    },
  };
}

/**
 * Start one native advertisement and, when a signal is provided, keep it alive
 * until cancellation.
 *
 * Cancellation wins even while native registration readiness is pending. The
 * caller settles promptly, while a handle that resolves later is immediately
 * closed. This keeps the peer and generic advertise surfaces aligned with the
 * cancellation guarantees of {@link createDiscoveryIterator}.
 *
 * @internal
 */
export async function runDiscoveryAdvertisement(
  options: DiscoveryAdvertisementOptions,
): Promise<void> {
  const { signal, start, close } = options;
  if (!signal) {
    await start();
    return;
  }
  if (signal.aborted) return;

  let resolveAbort!: () => void;
  const aborted = new Promise<void>((resolve) => {
    resolveAbort = resolve;
  });
  const onAbort = () => resolveAbort();
  let listenerRegistered = true;
  const removeAbortListener = () => {
    if (!listenerRegistered) return;
    listenerRegistered = false;
    signal.removeEventListener("abort", onAbort);
  };
  signal.addEventListener("abort", onAbort, { once: true });

  // Capture a synchronous adapter throw and keep observing this promise after
  // cancellation so a late rejection never becomes unhandled.
  const startPromise = Promise.resolve().then(start);
  let outcome:
    | { kind: "started"; handle: number }
    | { kind: "aborted" };
  try {
    outcome = await Promise.race([
      startPromise.then((handle) => ({ kind: "started" as const, handle })),
      aborted.then(() => ({ kind: "aborted" as const })),
    ]);
  } catch (error) {
    removeAbortListener();
    throw error;
  }

  if (outcome.kind === "aborted") {
    removeAbortListener();
    void startPromise.then(
      (handle) => {
        observeDetachedCleanup(
          cleanup(close, handle),
          "late discovery advertisement",
        );
      },
      () => {
        // Cancellation already settled the public operation. The start
        // rejection is deliberately observed and has no remaining owner.
      },
    );
    return;
  }

  await aborted;
  removeAbortListener();
  await cleanup(close, outcome.handle);
}

/**
 * Generic DNS-SD advertise/browse — the protocol-neutral engine underneath
 * iroh-http's own peer discovery. It has no state beyond the adapter it is
 * given, so it lives as plain functions rather than a class.
 *
 * Use it to announce and discover *any* local service, not just iroh nodes.
 * Records are lossless: instance label, host, port, socket addresses, and every
 * TXT property are preserved. iroh-http's `node.advertisePeer()` /
 * `node.browsePeers()` are thin specialisations of this same engine, and
 * `node.advertise()` / `node.browse()` expose it directly.
 */

/**
 * Advertise a generic DNS-SD service on the local network.
 *
 * Pass `options.signal` to control how long the service stays advertised: the
 * returned promise stays pending until the signal is aborted, then advertising
 * stops. Without a signal, the promise resolves immediately and the service
 * remains advertised for the lifetime of the process.
 *
 * ```ts
 * const ac = new AbortController();
 * const advertising = advertiseService(adapter, {
 *   serviceName: "my-app",
 *   instanceName: "living-room",
 *   port: 8080,
 *   txt: { version: "1" },
 *   signal: ac.signal,
 * });
 * // ... later:
 * ac.abort();
 * await advertising;
 * ```
 */
export function advertiseService(
  adapter: IrohAdapter,
  options: DnsSdAdvertiseOptions,
): Promise<void> {
  const { signal, ...config } = options;
  return runDiscoveryAdvertisement({
    signal,
    start: () => adapter.advertise(config as ServiceConfig),
    close: (handle) => adapter.advertiseClose(handle),
  });
}

/**
 * Browse for a generic DNS-SD service on the local network.
 *
 * Returns an async iterable that yields a {@link ServiceRecord} for each
 * announcement received. Like iroh's discovery semantics, records are **not**
 * de-duplicated: an active service re-announces periodically, and a service
 * that ages out is yielded once with `isActive: false`. The iterable ends when
 * `options.signal` is aborted or the session closes.
 *
 * ```ts
 * const ac = new AbortController();
 * for await (const rec of browseServices(adapter, { serviceName: "my-app", signal: ac.signal })) {
 *   console.log(rec.isActive ? "up" : "down", rec.instanceName, rec.addrs);
 * }
 * ```
 */
export function browseServices(
  adapter: IrohAdapter,
  options: DnsSdBrowseOptions,
): AsyncIterable<ServiceRecord> {
  const { serviceName, protocol, signal } = options;
  return createDiscoveryIterator<ServiceRecord, ServiceRecord>({
    signal,
    start: () => adapter.browse(serviceName, protocol),
    poll: (handle) => adapter.browseNext(handle),
    close: (handle) => adapter.browseClose(handle),
    map: (record) => record,
  });
}
