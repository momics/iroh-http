import { afterEach, describe, expect, it, vi } from "vitest";
import { withLifecycle, type LifecycleStorage } from "../lifecycle.ts";

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
});
