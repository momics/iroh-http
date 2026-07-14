import { afterEach, describe, expect, it, vi } from "vitest";
import {
  installForegroundHealthCheck,
  type LifecycleStorage,
  withLifecycle,
} from "../lifecycle.ts";

/** Flush a few microtask + macrotask turns so async handlers settle. */
async function flush(): Promise<void> {
  for (let i = 0; i < 5; i++) {
    await Promise.resolve();
  }
}

async function resolvesWithin(
  promise: Promise<unknown>,
  timeoutMs = 50,
): Promise<void> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timeout = setTimeout(
          () =>
            reject(
              new Error(`operation did not resolve within ${timeoutMs}ms`),
            ),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timeout !== undefined) clearTimeout(timeout);
  }
}

function memoryStorage(): LifecycleStorage & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    getItem: (key) => data.get(key) ?? null,
    setItem: (key, value) => {
      data.set(key, value);
    },
    removeItem: (key) => {
      data.delete(key);
    },
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("withLifecycle", () => {
  it("starts, persists desired running state, and stops with cleanup", async () => {
    const storage = memoryStorage();
    const cleanup = vi.fn();
    const states: string[] = [];
    let signal: AbortSignal | null = null;

    const handle = withLifecycle(
      "server",
      ({ signal: runSignal, reason, attempt }) => {
        signal = runSignal;
        expect(reason).toBe("start");
        expect(attempt).toBe(1);
        return cleanup;
      },
      { storage, onStateChange: (state) => states.push(state) },
    );

    await handle.start();

    expect(handle.state).toBe("running");
    expect(signal?.aborted).toBe(false);
    expect(storage.data.get("iroh-http-tauri:lifecycle:server")).toBe(
      "running",
    );

    await handle.stop();

    expect(cleanup).toHaveBeenCalledTimes(1);
    expect(signal?.aborted).toBe(true);
    expect(storage.data.has("iroh-http-tauri:lifecycle:server")).toBe(false);
    expect(handle.state).toBe("stopped");
    expect(states).toEqual(["starting", "running", "stopping", "stopped"]);
  });

  it("restores only when persisted desired state is running", async () => {
    const storage = memoryStorage();
    const run = vi.fn(() => undefined);
    const handle = withLifecycle("server", run, { storage });

    await handle.restore();
    expect(run).not.toHaveBeenCalled();

    storage.setItem("iroh-http-tauri:lifecycle:server", "running");
    await handle.restore();

    expect(run).toHaveBeenCalledTimes(1);
    expect(run.mock.calls[0][0]).toMatchObject({
      reason: "restore",
      attempt: 1,
    });
    expect(handle.state).toBe("running");
  });

  it("accepts closeable resources as cleanup", async () => {
    const storage = memoryStorage();
    const close = vi.fn();
    const handle = withLifecycle("resource", () => ({ close }), { storage });

    await handle.start();
    await handle.stop();

    expect(close).toHaveBeenCalledTimes(1);
  });

  it("restarts by aborting and cleaning up the current run", async () => {
    const storage = memoryStorage();
    const cleanup = vi.fn();
    const reasons: string[] = [];
    const signals: AbortSignal[] = [];
    const handle = withLifecycle(
      "server",
      ({ reason, signal }) => {
        reasons.push(reason);
        signals.push(signal);
        return cleanup;
      },
      { storage },
    );

    await handle.start();
    await handle.restart("manual");

    expect(reasons).toEqual(["start", "restart"]);
    expect(cleanup).toHaveBeenCalledTimes(1);
    expect(signals[0].aborted).toBe(true);
    expect(signals[1].aborted).toBe(false);
    expect(handle.state).toBe("running");
    expect(storage.data.get("iroh-http-tauri:lifecycle:server")).toBe(
      "running",
    );
  });

  it("restores on pageshow when desired state is running", async () => {
    const storage = memoryStorage();
    storage.setItem("iroh-http-tauri:lifecycle:server", "running");
    const run = vi.fn(() => undefined);
    const handle = withLifecycle("server", run, { storage });

    window.dispatchEvent(new PageTransitionEvent("pageshow"));
    await Promise.resolve();
    await Promise.resolve();

    expect(run).toHaveBeenCalledTimes(1);
    expect(run.mock.calls[0][0]).toMatchObject({
      reason: "visible",
      attempt: 1,
    });
    expect(handle.state).toBe("running");
  });

  it("surfaces start errors and does not persist failed starts", async () => {
    const storage = memoryStorage();
    const error = new Error("boom");
    const onError = vi.fn();
    const handle = withLifecycle(
      "server",
      () => {
        throw error;
      },
      { storage, onError },
    );

    await expect(handle.start()).rejects.toThrow("boom");

    expect(handle.state).toBe("error");
    expect(handle.error).toBe(error);
    expect(onError).toHaveBeenCalledWith(error);
    expect(storage.data.has("iroh-http-tauri:lifecycle:server")).toBe(false);
  });

  it("stop() unblocks a start whose run stays pending until aborted (#350 F4)", async () => {
    const storage = memoryStorage();
    let sawAbort = false;
    const handle = withLifecycle(
      "server",
      (ctx) =>
        // A signal-owned run that resolves only once its signal aborts.
        new Promise<() => void>((resolve) => {
          ctx.signal.addEventListener("abort", () => {
            sawAbort = true;
            resolve(() => {});
          });
        }),
      { storage },
    );

    const started = handle.start();
    await flush();
    expect(handle.state).toBe("starting");

    // Must not deadlock: stop aborts the in-flight start's controller so the
    // serialized queue can drain to the stop operation.
    await handle.stop();
    await started;

    expect(sawAbort).toBe(true);
    expect(handle.state).toBe("stopped");
  });

  it("stops a signal-owned start when start() and stop() are called in the same tick", async () => {
    const handle = withLifecycle(
      "same-tick-stop",
      ({ signal }) =>
        new Promise<void>((resolve) => {
          signal.addEventListener("abort", () => resolve(), { once: true });
        }),
      { storage: null },
    );

    const started = handle.start();
    const stopped = handle.stop();

    await expect(resolvesWithin(Promise.all([started, stopped]))).resolves
      .toBeUndefined();
    expect(handle.state).toBe("stopped");
    expect(handle.signal).toBeNull();
  });

  it("restarts a signal-owned start when start() and restart() are called in the same tick", async () => {
    const handle = withLifecycle(
      "same-tick-restart",
      ({ signal, reason }) => {
        if (reason === "start") {
          return new Promise<void>((resolve) => {
            signal.addEventListener("abort", () => resolve(), { once: true });
          });
        }
        return () => {};
      },
      { storage: null },
    );

    const started = handle.start();
    const restarted = handle.restart();

    await expect(resolvesWithin(Promise.all([started, restarted]))).resolves
      .toBeUndefined();
    expect(handle.state).toBe("running");
    expect(handle.signal?.aborted).toBe(false);
    await handle.close();
  });

  it("closes a signal-owned start when start() and close() are called in the same tick", async () => {
    const handle = withLifecycle(
      "same-tick-close",
      ({ signal }) =>
        new Promise<void>((resolve) => {
          signal.addEventListener("abort", () => resolve(), { once: true });
        }),
      { storage: null },
    );

    const started = handle.start();
    const closed = handle.close();

    await expect(resolvesWithin(Promise.all([started, closed]))).resolves
      .toBeUndefined();
    expect(handle.state).toBe("stopped");
    expect(handle.signal).toBeNull();
    await expect(handle.start()).rejects.toThrow("handle is closed");
  });

  it("aborts the signal when a start fails after taking the signal (#350 F15)", async () => {
    const storage = memoryStorage();
    let capturedSignal: AbortSignal | undefined;
    const handle = withLifecycle(
      "server",
      (ctx) => {
        capturedSignal = ctx.signal;
        throw new Error("late failure");
      },
      { storage },
    );

    await expect(handle.start()).rejects.toThrow("late failure");

    // The failed start must have aborted the signal it handed to `run`, and
    // must not leave a dangling controller behind.
    expect(capturedSignal?.aborted).toBe(true);
    expect(handle.signal).toBeNull();
  });

  it("calls onError once for a visibility-triggered start failure (#350 F30)", async () => {
    const storage = memoryStorage();
    storage.setItem("iroh-http-tauri:lifecycle:server", "running");
    const onError = vi.fn();
    const error = new Error("visible boom");
    withLifecycle(
      "server",
      () => {
        throw error;
      },
      { storage, onError },
    );

    document.dispatchEvent(new Event("visibilitychange"));
    await flush();

    expect(onError).toHaveBeenCalledTimes(1);
    expect(onError).toHaveBeenCalledWith(error);
  });
});

describe("installForegroundHealthCheck", () => {
  it("does not report an in-flight failed probe after the check is removed", async () => {
    let resolveProbe!: (healthy: boolean) => void;
    const probeResult = new Promise<boolean>((resolve) => {
      resolveProbe = resolve;
    });
    const onUnhealthy = vi.fn();
    const remove = installForegroundHealthCheck({
      probe: () => probeResult,
      onUnhealthy,
      enabled: true,
      maxRetries: 1,
    });

    document.dispatchEvent(new Event("visibilitychange"));
    await flush();
    remove();
    resolveProbe(false);
    await flush();

    expect(onUnhealthy).not.toHaveBeenCalled();
  });

  it("stops a retry sweep when removed during backoff", async () => {
    vi.useFakeTimers();
    try {
      let probeAttempts = 0;
      const onUnhealthy = vi.fn();
      const remove = installForegroundHealthCheck({
        probe: () => {
          probeAttempts += 1;
          return Promise.resolve(false);
        },
        onUnhealthy,
        enabled: true,
        maxRetries: 2,
        backoffMs: () => 100,
      });

      document.dispatchEvent(new Event("visibilitychange"));
      await flush();
      expect(probeAttempts).toBe(1);

      remove();
      await vi.advanceTimersByTimeAsync(100);
      await flush();

      expect(probeAttempts).toBe(1);
      expect(onUnhealthy).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("hands off an unhealthy transport only once", async () => {
    let replacements = 0;
    const remove = installForegroundHealthCheck({
      probe: () => Promise.resolve(false),
      onUnhealthy: () => {
        replacements += 1;
      },
      enabled: true,
      maxRetries: 1,
    });

    document.dispatchEvent(new Event("visibilitychange"));
    await flush();
    expect(replacements).toBe(1);

    window.dispatchEvent(new PageTransitionEvent("pageshow"));
    await flush();
    expect(replacements).toBe(1);
    remove();
  });

  it("triggers recovery when the foreground transport probe fails", async () => {
    // Regression for #336: a foreground event fires while the endpoint handle
    // still exists, but the transport health probe reports the transport is no
    // longer usable. Recovery must be triggered — not treated as healthy.
    const onUnhealthy = vi.fn();
    let healthy = true;
    const remove = installForegroundHealthCheck({
      probe: () => Promise.resolve(healthy),
      onUnhealthy,
      enabled: true,
      maxRetries: 2,
      backoffMs: () => 0,
    });

    healthy = false;
    document.dispatchEvent(new Event("visibilitychange"));
    await flush();

    expect(onUnhealthy).toHaveBeenCalledTimes(1);
    remove();
  });

  it("treats a thrown probe (handle gone) as unhealthy", async () => {
    const onUnhealthy = vi.fn();
    const remove = installForegroundHealthCheck({
      probe: () => Promise.reject(new Error("INVALID_HANDLE")),
      onUnhealthy,
      enabled: true,
      maxRetries: 1,
      backoffMs: () => 0,
    });

    document.dispatchEvent(new Event("visibilitychange"));
    await flush();

    expect(onUnhealthy).toHaveBeenCalledTimes(1);
    remove();
  });

  it("does not trigger recovery while the transport stays healthy", async () => {
    const onUnhealthy = vi.fn();
    const remove = installForegroundHealthCheck({
      probe: () => Promise.resolve(true),
      onUnhealthy,
      enabled: true,
      maxRetries: 2,
      backoffMs: () => 0,
    });

    document.dispatchEvent(new Event("visibilitychange"));
    await flush();

    expect(onUnhealthy).not.toHaveBeenCalled();
    remove();
  });

  it("is inert when disabled", async () => {
    const probe = vi.fn(() => Promise.resolve(false));
    const onUnhealthy = vi.fn();
    const remove = installForegroundHealthCheck({
      probe,
      onUnhealthy,
      enabled: false,
    });

    // Returns a no-op remover and never wires listeners.
    document.dispatchEvent(new Event("visibilitychange"));
    await flush();

    expect(probe).not.toHaveBeenCalled();
    expect(onUnhealthy).not.toHaveBeenCalled();
    remove();
  });
});

describe("reconnect policy (#336)", () => {
  function memoryStorage(): LifecycleStorage & { data: Map<string, string> } {
    const data = new Map<string, string>();
    return {
      data,
      getItem: (key) => data.get(key) ?? null,
      setItem: (key, value) => {
        data.set(key, value);
      },
      removeItem: (key) => {
        data.delete(key);
      },
    };
  }

  it(
    "restarts a running serve task cleanly when foreground health fails, " +
      "leaving no half-running state",
    async () => {
      const storage = memoryStorage();
      const cleanups: number[] = [];
      const signals: AbortSignal[] = [];
      let attempts = 0;

      const serve = withLifecycle(
        "server",
        ({ signal }) => {
          signals.push(signal);
          attempts += 1;
          const n = attempts;
          return () => {
            cleanups.push(n);
          };
        },
        { storage },
      );

      await serve.start();
      expect(serve.state).toBe("running");
      expect(attempts).toBe(1);

      // Foreground health probe reports the transport is dead; the reconnect
      // policy tears the old run down and restarts it.
      let recovery: Promise<void> = Promise.resolve();
      const remove = installForegroundHealthCheck({
        probe: () => Promise.resolve(false),
        onUnhealthy: () => {
          recovery = serve.restart("foreground-health-failed");
        },
        enabled: true,
        maxRetries: 1,
        backoffMs: () => 0,
      });

      document.dispatchEvent(new Event("visibilitychange"));
      await flush();
      await recovery;

      // The first run was cleaned up and its signal aborted — nothing is left
      // half-running — and a fresh run is active.
      expect(cleanups).toContain(1);
      expect(signals[0].aborted).toBe(true);
      expect(attempts).toBe(2);
      expect(signals[1].aborted).toBe(false);
      expect(serve.state).toBe("running");

      remove();
      await serve.close();
    },
  );
});
