export type LifecycleState =
  | "stopped"
  | "starting"
  | "running"
  | "recovering"
  | "stopping"
  | "error";

export type LifecycleStartReason =
  | "start"
  | "restore"
  | "restart"
  | "visible";

export interface LifecycleContext {
  signal: AbortSignal;
  reason: LifecycleStartReason;
  attempt: number;
}

export type LifecycleResource =
  | void
  | null
  | (() => void | Promise<void>)
  | {
    close?: () => void | Promise<void>;
  };

export interface LifecycleStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface LifecycleOptions {
  /**
   * Storage used to remember whether this lifecycle task should be restored
   * after a WebView reload. Defaults to `window.localStorage` when available.
   */
  storage?: LifecycleStorage | null;

  /**
   * Namespace for the persisted storage key.
   * @default "iroh-http-tauri:lifecycle"
   */
  storagePrefix?: string;

  /**
   * Re-run the task when the document becomes visible and the persisted desired
   * state says it should be running.
   * @default true
   */
  restoreOnVisible?: boolean;

  /**
   * Called whenever the task state changes.
   */
  onStateChange?: (state: LifecycleState) => void;

  /**
   * Called when the wrapped task throws during start/restore/restart.
   */
  onError?: (error: unknown) => void;
}

export interface LifecycleHandle {
  readonly id: string;
  readonly state: LifecycleState;
  readonly error: unknown;
  readonly signal: AbortSignal | null;
  start(): Promise<void>;
  restore(): Promise<void>;
  restart(reason?: string): Promise<void>;
  stop(): Promise<void>;
  close(): Promise<void>;
}

const DEFAULT_STORAGE_PREFIX = "iroh-http-tauri:lifecycle";
const RUNNING_VALUE = "running";

export function withLifecycle(
  id: string,
  run: (
    ctx: LifecycleContext,
  ) => LifecycleResource | Promise<LifecycleResource>,
  options: LifecycleOptions = {},
): LifecycleHandle {
  const lifecycleId = id.trim();
  if (!lifecycleId) {
    throw new TypeError("withLifecycle() requires a non-empty id");
  }

  const storage = options.storage === undefined
    ? defaultStorage()
    : options.storage;
  const storageKey = `${
    options.storagePrefix ?? DEFAULT_STORAGE_PREFIX
  }:${lifecycleId}`;
  const restoreOnVisible = options.restoreOnVisible ?? true;

  let state: LifecycleState = "stopped";
  let error: unknown;
  let controller: AbortController | null = null;
  let cleanup: (() => void | Promise<void>) | null = null;
  let disposed = false;
  let attempt = 0;
  let operation: Promise<void> = Promise.resolve();

  const setState = (next: LifecycleState): void => {
    state = next;
    options.onStateChange?.(next);
  };

  const persistRunning = (): void => {
    storage?.setItem(storageKey, RUNNING_VALUE);
  };

  const clearPersisted = (): void => {
    storage?.removeItem(storageKey);
  };

  const shouldRestore = (): boolean =>
    storage?.getItem(storageKey) === RUNNING_VALUE;

  const runCleanup = async (): Promise<void> => {
    const currentCleanup = cleanup;
    cleanup = null;
    const currentController = controller;
    controller = null;
    currentController?.abort();
    if (currentCleanup) {
      await currentCleanup();
    }
  };

  const normalizeCleanup = (
    resource: LifecycleResource,
  ): (() => void | Promise<void>) | null => {
    if (!resource) return null;
    if (typeof resource === "function") return resource;
    if (typeof resource.close === "function") {
      return () => resource.close!();
    }
    return null;
  };

  const startInternal = async (
    reason: LifecycleStartReason,
    persist: boolean,
  ): Promise<void> => {
    if (disposed) {
      throw new Error("withLifecycle() handle is closed");
    }
    if (state === "running" || state === "starting" || state === "recovering") {
      return;
    }

    error = undefined;
    setState(reason === "start" ? "starting" : "recovering");
    const nextController = new AbortController();
    controller = nextController;
    const nextAttempt = ++attempt;

    try {
      const resource = await run({
        signal: nextController.signal,
        reason,
        attempt: nextAttempt,
      });
      if (nextController.signal.aborted) {
        await normalizeCleanup(resource)?.();
        return;
      }
      cleanup = normalizeCleanup(resource);
      if (persist) persistRunning();
      setState("running");
    } catch (err) {
      if (controller === nextController) {
        controller = null;
      }
      error = err;
      setState("error");
      options.onError?.(err);
      throw err;
    }
  };

  const enqueue = (fn: () => Promise<void>): Promise<void> => {
    operation = operation.then(fn, fn);
    return operation;
  };

  const handle: LifecycleHandle = {
    id: lifecycleId,
    get state() {
      return state;
    },
    get error() {
      return error;
    },
    get signal() {
      return controller?.signal ?? null;
    },
    start() {
      return enqueue(() => startInternal("start", true));
    },
    restore() {
      return enqueue(async () => {
        if (!shouldRestore()) return;
        await startInternal("restore", true);
      });
    },
    restart(_reason?: string) {
      return enqueue(async () => {
        setState("stopping");
        await runCleanup();
        await startInternal("restart", true);
      });
    },
    stop() {
      return enqueue(async () => {
        clearPersisted();
        if (state === "stopped") return;
        setState("stopping");
        await runCleanup();
        setState("stopped");
      });
    },
    close() {
      return enqueue(async () => {
        disposed = true;
        clearPersisted();
        removeVisibilityListener();
        if (state !== "stopped") {
          setState("stopping");
          await runCleanup();
        }
        setState("stopped");
      });
    },
  };

  const visibilityHandler = (): void => {
    if (!restoreOnVisible || disposed) return;
    if (
      typeof document !== "undefined" && document.visibilityState !== "visible"
    ) {
      return;
    }
    if (!shouldRestore()) return;
    void enqueue(() => startInternal("visible", true)).catch((err) => {
      options.onError?.(err);
    });
  };

  const removeVisibilityListener = installVisibilityListener(visibilityHandler);

  return handle;
}

/**
 * Options for {@link installForegroundHealthCheck}.
 */
export interface ForegroundHealthCheckOptions {
  /**
   * Probe the underlying transport. Resolve `true` when the transport is still
   * usable, or resolve `false` / reject when it is not. This must check real
   * transport liveness — not merely that the endpoint handle exists (#336).
   */
  probe: () => Promise<boolean>;

  /**
   * Called when the probe reports the transport is unusable after exhausting
   * `maxRetries`. This is the signal to tear down and recreate the node.
   */
  onUnhealthy: () => void;

  /**
   * Whether to install the listener at all. Callers gate this on mobile
   * user-agent detection or an explicit opt-in.
   * @default true
   */
  enabled?: boolean;

  /**
   * Maximum probe attempts per foreground event before declaring the transport
   * dead.
   * @default 3
   */
  maxRetries?: number;

  /**
   * Backoff (ms) before the retry following attempt `n` (1-based). A value of
   * `0` skips the delay entirely.
   * @default (n) => 100 * 2 ** n
   */
  backoffMs?: (attempt: number) => number;
}

/**
 * Run a transport health probe whenever the document returns to the foreground
 * and trigger `onUnhealthy` when the transport is no longer usable.
 *
 * On iOS the OS can invalidate an endpoint's socket while the app is
 * suspended. When the app foregrounds, the endpoint *handle* still exists but
 * the transport is dead. A handle-existence check (the old `ping`) reports
 * healthy in this state, so `serve()` restarts on a dead endpoint and remote
 * peers hang until a long timeout (#336). This wires a real health contract to
 * the foreground event so recovery can be triggered deterministically.
 *
 * @returns a function that removes the installed listeners.
 */
export function installForegroundHealthCheck(
  options: ForegroundHealthCheckOptions,
): () => void {
  const {
    probe,
    onUnhealthy,
    enabled = true,
    maxRetries = 3,
    backoffMs = (attempt) => 100 * 2 ** attempt,
  } = options;

  if (!enabled || typeof document === "undefined") {
    return () => {};
  }

  let running = false;
  const handler = async (): Promise<void> => {
    if (
      typeof document !== "undefined" && document.visibilityState !== "visible"
    ) {
      return;
    }
    // Coalesce overlapping foreground events into a single probe sweep.
    if (running) return;
    running = true;
    try {
      for (let attempt = 1; attempt <= maxRetries; attempt++) {
        let healthy = false;
        try {
          healthy = await probe();
        } catch {
          healthy = false;
        }
        if (healthy) return;
        if (attempt < maxRetries) {
          const delay = Math.max(0, backoffMs(attempt));
          if (delay > 0) {
            await new Promise<void>((resolve) => setTimeout(resolve, delay));
          }
        }
      }
      onUnhealthy();
    } finally {
      running = false;
    }
  };

  const listener = (): void => {
    void handler();
  };
  document.addEventListener("visibilitychange", listener);
  if (typeof window !== "undefined") {
    window.addEventListener("pageshow", listener);
  }
  return () => {
    document.removeEventListener("visibilitychange", listener);
    if (typeof window !== "undefined") {
      window.removeEventListener("pageshow", listener);
    }
  };
}

function defaultStorage(): LifecycleStorage | null {
  try {
    return typeof window !== "undefined" ? window.localStorage : null;
  } catch {
    return null;
  }
}

function installVisibilityListener(handler: () => void): () => void {
  if (typeof document === "undefined" || typeof window === "undefined") {
    return () => {};
  }
  document.addEventListener("visibilitychange", handler);
  window.addEventListener("pageshow", handler);
  return () => {
    document.removeEventListener("visibilitychange", handler);
    window.removeEventListener("pageshow", handler);
  };
}
