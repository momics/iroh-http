import type { IrohAdapter } from "./IrohAdapter.js";
import type {
  DnsSdAdvertiseOptions,
  DnsSdBrowseOptions,
  ServiceConfig,
  ServiceRecord,
} from "./discovery.js";

/**
 * Generic DNS-SD advertise/browse — the protocol-neutral surface underneath
 * iroh-http's own peer discovery.
 *
 * Use it to announce and discover *any* local service, not just iroh nodes.
 * Records are lossless: instance label, host, port, socket addresses, and every
 * TXT property are preserved. iroh-http's `node.advertise()` / `node.browse()`
 * are thin specialisations of this same engine.
 */
export class DnsSd {
  readonly #adapter: IrohAdapter;

  constructor(adapter: IrohAdapter) {
    this.#adapter = adapter;
  }

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
   * const advertising = dnsSd.advertise({
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
  async advertise(options: DnsSdAdvertiseOptions): Promise<void> {
    const { signal, ...config } = options;
    const advHandle = await this.#adapter.dnsSdAdvertise(
      config as ServiceConfig,
    );
    if (signal) {
      return new Promise<void>((resolve) => {
        const stop = () => {
          this.#adapter.dnsSdAdvertiseClose(advHandle);
          resolve();
        };
        if (signal.aborted) {
          stop();
          return;
        }
        signal.addEventListener("abort", stop, { once: true });
      });
    }
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
   * for await (const rec of dnsSd.browse({ serviceName: "my-app", signal: ac.signal })) {
   *   console.log(rec.isActive ? "up" : "down", rec.instanceName, rec.addrs);
   * }
   * ```
   */
  browse(options: DnsSdBrowseOptions): AsyncIterable<ServiceRecord> {
    const adapter = this.#adapter;
    const { serviceName, protocol, signal } = options;

    return {
      [Symbol.asyncIterator]() {
        let browseHandle: number | null = null;
        return {
          async next(): Promise<IteratorResult<ServiceRecord>> {
            if (browseHandle === null) {
              browseHandle = await adapter.dnsSdBrowse(serviceName, protocol);
            }
            if (signal?.aborted) {
              adapter.dnsSdBrowseClose(browseHandle);
              browseHandle = null;
              return { done: true as const, value: undefined };
            }

            let record: ServiceRecord | null;
            if (signal) {
              const abortPromise = new Promise<null>((resolve) => {
                if (signal.aborted) {
                  resolve(null);
                  return;
                }
                signal.addEventListener("abort", () => resolve(null), {
                  once: true,
                });
              });
              record = await Promise.race([
                adapter.dnsSdNextRecord(browseHandle),
                abortPromise,
              ]);
              if (signal.aborted && browseHandle !== null) {
                adapter.dnsSdBrowseClose(browseHandle);
                browseHandle = null;
                return { done: true as const, value: undefined };
              }
            } else {
              record = await adapter.dnsSdNextRecord(browseHandle);
            }

            if (record === null) {
              return { done: true as const, value: undefined };
            }
            return { done: false as const, value: record };
          },
          return(): Promise<IteratorResult<ServiceRecord>> {
            if (browseHandle !== null) {
              adapter.dnsSdBrowseClose(browseHandle);
              browseHandle = null;
            }
            return Promise.resolve({ done: true as const, value: undefined });
          },
        };
      },
    };
  }
}
