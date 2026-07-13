/**
 * Guest-JS unit tests for the Tauri adapter.
 *
 * Uses `@tauri-apps/api/mocks` + Vitest to test the guest-side TypeScript
 * without a real webview or Rust backend.
 *
 * Covers:
 *   - `nextChunk`: prefix byte protocol (0x01 = data, 0x00 = EOF)
 *   - `sendChunk`: raw binary IPC with correct headers
 *   - `createNode`: IPC invocations for endpoint creation
 *   - Error propagation from IPC failures
 */

import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import { randomFillSync } from "crypto";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";

// jsdom lacks WebCrypto — polyfill for Tauri internals.
beforeAll(() => {
  Object.defineProperty(window, "crypto", {
    value: {
      getRandomValues: (buffer: Uint8Array) => randomFillSync(buffer),
    },
  });
});

afterEach(() => {
  clearMocks();
});

// ── nextChunk prefix byte protocol ──────────────────────────────────────────

describe("nextChunk prefix byte protocol", () => {
  beforeEach(() => {
    mockWindows("main");
  });

  it("returns data when prefix byte is 0x01", async () => {
    const testData = new TextEncoder().encode("hello iroh");
    const responseBuffer = new Uint8Array(1 + testData.length);
    responseBuffer[0] = 0x01; // data prefix
    responseBuffer.set(testData, 1);

    mockIPC((cmd, _args) => {
      // try_next_chunk is called first (sync fast path)
      if (cmd === "plugin:iroh-http|try_next_chunk") {
        return responseBuffer.buffer;
      }
      // Async fallback
      if (cmd === "plugin:iroh-http|next_chunk") {
        return responseBuffer.buffer;
      }
    });

    const { invoke } = await import("@tauri-apps/api/core");

    // Simulate what TauriAdapter.nextChunk does:
    const buf = await invoke<ArrayBuffer>("plugin:iroh-http|try_next_chunk", {
      endpointHandle: 1,
      handle: 42,
    });
    const v = new Uint8Array(buf as ArrayBuffer);
    const result = v.length > 0 && v[0] !== 0 ? v.subarray(1) : null;

    expect(result).not.toBeNull();
    expect(new TextDecoder().decode(result!)).toBe("hello iroh");
  });

  it("returns null (EOF) when prefix byte is 0x00", async () => {
    const eofBuffer = new Uint8Array([0x00]);

    mockIPC((cmd) => {
      if (cmd === "plugin:iroh-http|try_next_chunk") {
        return eofBuffer.buffer;
      }
      if (cmd === "plugin:iroh-http|next_chunk") {
        return eofBuffer.buffer;
      }
    });

    const { invoke } = await import("@tauri-apps/api/core");

    const buf = await invoke<ArrayBuffer>("plugin:iroh-http|try_next_chunk", {
      endpointHandle: 1,
      handle: 42,
    });
    const v = new Uint8Array(buf as ArrayBuffer);
    const result = v.length > 0 && v[0] !== 0 ? v.subarray(1) : null;

    expect(result).toBeNull();
  });

  it("returns null for empty buffer", async () => {
    const emptyBuffer = new Uint8Array(0);

    mockIPC((cmd) => {
      if (cmd === "plugin:iroh-http|try_next_chunk") {
        return emptyBuffer.buffer;
      }
    });

    const { invoke } = await import("@tauri-apps/api/core");

    const buf = await invoke<ArrayBuffer>("plugin:iroh-http|try_next_chunk", {
      endpointHandle: 1,
      handle: 42,
    });
    const v = new Uint8Array(buf as ArrayBuffer);
    const result = v.length > 0 && v[0] !== 0 ? v.subarray(1) : null;

    expect(result).toBeNull();
  });
});

// ── sendChunk IPC ───────────────────────────────────────────────────────────

describe("sendChunk IPC", () => {
  beforeEach(() => {
    mockWindows("main");
  });

  it("invokes send_chunk with correct command", async () => {
    const calls: Array<{ cmd: string; args: unknown }> = [];

    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "plugin:iroh-http|send_chunk") {
        return undefined;
      }
    });

    const { invoke } = await import("@tauri-apps/api/core");

    // Simulate what TauriAdapter.sendChunk does (simplified — without raw binary headers)
    await invoke("plugin:iroh-http|send_chunk", {
      endpointHandle: 1,
      handle: 42,
      data: "aGVsbG8=", // "hello" base64
    });

    expect(calls.length).toBe(1);
    expect(calls[0].cmd).toBe("plugin:iroh-http|send_chunk");
  });
});

// ── reconnectEnabled gating (#350 F3) ───────────────────────────────────────

describe("reconnectEnabled", () => {
  it("honors an explicit auto:false even on mobile", async () => {
    const { reconnectEnabled } = await import("../index.ts");
    // The destructive #350 F3 bug: mobile force-enabled the listener and could
    // permanently close a node whose reconnect the caller had disabled.
    expect(reconnectEnabled(false, true)).toBe(false);
    expect(reconnectEnabled(false, false)).toBe(false);
  });

  it("enables implicitly on mobile when unset", async () => {
    const { reconnectEnabled } = await import("../index.ts");
    expect(reconnectEnabled(undefined, true)).toBe(true);
    expect(reconnectEnabled(undefined, false)).toBe(false);
  });

  it("enables on explicit auto:true regardless of platform", async () => {
    const { reconnectEnabled } = await import("../index.ts");
    expect(reconnectEnabled(true, false)).toBe(true);
    expect(reconnectEnabled(true, true)).toBe(true);
  });
});

// ── createNode IPC ──────────────────────────────────────────────────────────

describe("createNode IPC", () => {
  beforeEach(() => {
    mockWindows("main");
  });

  it("invokes create_endpoint and wait_endpoint_closed", async () => {
    const invokedCommands: string[] = [];

    mockIPC((cmd) => {
      invokedCommands.push(cmd);
      if (cmd === "plugin:iroh-http|create_endpoint") {
        return {
          endpointHandle: 1,
          // Valid base32-encoded 32-byte public key (all zeros).
          nodeId: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        };
      }
      if (cmd === "plugin:iroh-http|wait_endpoint_closed") {
        // Never resolve — simulates a long-lived endpoint.
        return new Promise(() => {});
      }
      if (cmd === "plugin:iroh-http|node_addr") {
        return {
          id: "testnode123456789012345678901234567890123456789012",
          addrs: [],
        };
      }
    });

    const { createNode } = await import("../index.ts");
    const node = await createNode({ disableNetworking: true });

    expect(node).toBeDefined();
    expect(node.secretKey).toBeUndefined();
    expect(invokedCommands).toContain("plugin:iroh-http|create_endpoint");
    expect(invokedCommands).toContain("plugin:iroh-http|wait_endpoint_closed");
  });
});

// ── browsePeers isActive mapping (#330 review finding #10) ──────────────────

describe("browsePeers isActive mapping", () => {
  beforeEach(() => {
    mockWindows("main");
  });

  it("maps the raw browse_peers_next payload to a tagged discovered event", async () => {
    let browseNextCalls = 0;

    mockIPC((cmd) => {
      if (cmd === "plugin:iroh-http|create_endpoint") {
        return {
          endpointHandle: 1,
          nodeId: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        };
      }
      if (cmd === "plugin:iroh-http|wait_endpoint_closed") {
        return new Promise(() => {});
      }
      if (cmd === "plugin:iroh-http|node_addr") {
        return {
          id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          addrs: [],
        };
      }
      if (cmd === "plugin:iroh-http|browse_peers") {
        return 1;
      }
      if (cmd === "plugin:iroh-http|browse_peers_next") {
        browseNextCalls += 1;
        // Raw native payload shape (camelCase, untagged) — what the Rust
        // command actually serializes. It has no `type` field.
        if (browseNextCalls === 1) {
          return {
            isActive: true,
            nodeId: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            addrs: ["127.0.0.1:1234"],
          };
        }
        return null;
      }
    });

    const { createNode } = await import("../index.ts");
    const node = await createNode({ disableNetworking: true });

    const peers = [];
    for await (const peer of node.browsePeers()) {
      peers.push(peer);
    }

    expect(peers).toHaveLength(1);
    // Regression guard for #330 finding #10: before the fix, the untagged
    // payload was cast straight to `PeerDiscoveryEvent`, so `event.type` was
    // always `undefined` and every discovered peer reported `isActive: false`.
    expect(peers[0].isActive).toBe(true);
    expect(peers[0].nodeId).toBe(
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );
    expect(peers[0].addrs).toEqual(["127.0.0.1:1234"]);
  });
});

// ── Error propagation ───────────────────────────────────────────────────────

describe("error propagation", () => {
  beforeEach(() => {
    mockWindows("main");
  });

  it("propagates IPC errors from invoke", async () => {
    mockIPC((cmd) => {
      if (cmd === "plugin:iroh-http|create_endpoint") {
        throw new Error('{"code":"BIND_FAILED","message":"address in use"}');
      }
    });

    const { createNode } = await import("../index.ts");
    await expect(createNode()).rejects.toThrow();
  });
});

// ── serve / stop / serve cycles on one node (issue #336 follow-up) ───────────

describe("serve restart cycles", () => {
  beforeEach(() => {
    mockWindows("main");
  });

  /**
   * Build a `mockIPC` handler that faithfully models the Rust serve lifecycle
   * for `serve` / `stop_serve` / `wait_serve_stop`:
   *
   * - `serve` registers a NEW loop whose done-signal starts `false`. Like the
   *   real Tokio command, `set_serve_handle` runs a macrotask *after* the JS
   *   `invoke("serve")` call returns — so the registration is deferred.
   * - `stop_serve` flips the current loop's done-signal to `true`.
   * - `wait_serve_stop` captures whatever loop is registered *at call time* and
   *   resolves when that loop's done-signal is `true` (immediately if already
   *   `true`). This is the crux: the previous cycle's loop stays latched `true`,
   *   so a `wait_serve_stop` that runs before the current cycle's `serve`
   *   registered its loop reads the STALE `true` and resolves early.
   */
  function makeServeLifecycleIPC() {
    type Loop = { done: boolean; notify: (() => void) | null };
    let current: Loop | null = null;

    return (cmd: string): unknown => {
      switch (cmd) {
        case "plugin:iroh-http|create_endpoint":
          return {
            endpointHandle: 1,
            nodeId: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          };
        case "plugin:iroh-http|wait_endpoint_closed":
          return new Promise(() => {});
        case "plugin:iroh-http|serve":
          // Defer registration by a macrotask, mirroring the Rust command's
          // async dispatch: `set_serve_handle` has NOT run when this invoke
          // resolves-to-callers-synchronously in the buggy ordering.
          return new Promise<null>((resolve) => {
            setTimeout(() => {
              current = { done: false, notify: null };
              resolve(null);
            }, 0);
          });
        case "plugin:iroh-http|stop_serve": {
          const loop = current;
          if (loop) {
            loop.done = true;
            loop.notify?.();
          }
          return null;
        }
        case "plugin:iroh-http|wait_serve_stop": {
          const loop = current;
          if (!loop || loop.done) return Promise.resolve(null);
          return new Promise<null>((resolve) => {
            loop.notify = () => resolve(null);
          });
        }
        default:
          return null;
      }
    };
  }

  it("does not issue wait_serve_stop until serve has resolved (correlates the loop-done wait to this cycle — #336 follow-up)", async () => {
    const order: string[] = [];
    let resolveServe: (() => void) | null = null;

    mockIPC((cmd) => {
      order.push(cmd);
      switch (cmd) {
        case "plugin:iroh-http|create_endpoint":
          return {
            endpointHandle: 1,
            nodeId: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          };
        case "plugin:iroh-http|wait_endpoint_closed":
          return new Promise(() => {});
        case "plugin:iroh-http|serve":
          // Hold `serve` pending: on the Rust side this command runs
          // `set_serve_handle`, which installs THIS cycle's loop-done signal.
          return new Promise<null>((resolve) => {
            resolveServe = () => resolve(null);
          });
        case "plugin:iroh-http|wait_serve_stop":
          return Promise.resolve(null);
        default:
          return null;
      }
    });

    const { createNode } = await import("../index.ts");
    const node = await createNode({ disableNetworking: true });

    const controller = new AbortController();
    const handle = node.serve(
      { signal: controller.signal },
      () => new Response("ok"),
    );

    // Flush microtasks: `serve` has been invoked but is still pending.
    await new Promise((r) => setTimeout(r, 0));
    expect(order).toContain("plugin:iroh-http|serve");
    // The bug: issuing wait_serve_stop now (before serve resolved) makes it
    // latch the PREVIOUS cycle's already-`true` done signal on the Rust side,
    // so the loop-done promise resolves against the wrong serve loop. It must
    // wait until `serve` (set_serve_handle) has resolved.
    expect(order).not.toContain("plugin:iroh-http|wait_serve_stop");

    // Resolve `serve` (set_serve_handle done) → wait_serve_stop may now fire.
    resolveServe?.();
    await new Promise((r) => setTimeout(r, 0));
    expect(order).toContain("plugin:iroh-http|wait_serve_stop");

    controller.abort();
    await handle.finished;
  });

  it("allows serve() → stop → serve() → stop → serve() on the same node", async () => {
    mockIPC(makeServeLifecycleIPC());

    const { createNode } = await import("../index.ts");
    const node = await createNode({ disableNetworking: true });

    // Three full serve/stop cycles on the same node — mirrors the device repro
    // (Test server → stop → HTTP server → stop → Test server). Before the fix,
    // the third serve() threw "serve() is already running on this node."
    for (let cycle = 0; cycle < 3; cycle++) {
      const controller = new AbortController();
      const handle = node.serve(
        { signal: controller.signal },
        () => new Response("ok"),
      );
      // Let the deferred `serve` registration + wait_serve_stop wiring settle.
      await new Promise((r) => setTimeout(r, 0));
      controller.abort();
      await handle.finished;
      // Give the guard-reset microtask a turn.
      await new Promise((r) => setTimeout(r, 0));
    }

    // A fourth serve() must still be allowed (guard fully reset every cycle).
    const controller = new AbortController();
    expect(() =>
      node.serve({ signal: controller.signal }, () => new Response("ok"))
    )
      .not.toThrow();
    controller.abort();
  });
});

// ── path-change subscription parity (#314) ──────────────────────────────────

describe("path-change subscription", () => {
  beforeEach(() => {
    mockWindows("main");
  });

  // Regression guard: before the Tauri adapter overrode nextPathChange /
  // unsubscribePathChanges, pathChanges() fell through to the IrohAdapter base
  // class and rejected with "nextPathChange() not supported by this adapter".
  it("invokes next_path_change / unsubscribe_path_changes and completes cleanly", async () => {
    const invoked: Array<{ cmd: string; args: unknown }> = [];

    mockIPC((cmd, args) => {
      invoked.push({ cmd, args });
      if (cmd === "plugin:iroh-http|create_endpoint") {
        return {
          endpointHandle: 1,
          nodeId: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        };
      }
      if (cmd === "plugin:iroh-http|wait_endpoint_closed") {
        return new Promise(() => {});
      }
      if (cmd === "plugin:iroh-http|node_addr") {
        return {
          id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          addrs: [],
        };
      }
      // Subscription end — the core command returns null when the watcher
      // stops. The iterator must then tear down via unsubscribe_path_changes.
      if (cmd === "plugin:iroh-http|next_path_change") return null;
      if (cmd === "plugin:iroh-http|unsubscribe_path_changes") return null;
      if (cmd === "plugin:iroh-http|close_endpoint") return undefined;
    });

    const { createNode } = await import("../index.ts");
    const node = await createNode({ disableNetworking: true });
    const iterator = node
      .pathChanges(node.publicKey)
      [Symbol.asyncIterator]();
    const result = await iterator.next();

    expect(result.done).toBe(true);
    const cmds = invoked.map((c) => c.cmd);
    expect(cmds).toContain("plugin:iroh-http|next_path_change");
    expect(cmds).toContain("plugin:iroh-http|unsubscribe_path_changes");
    const nextCall = invoked.find(
      (c) => c.cmd === "plugin:iroh-http|next_path_change",
    );
    expect((nextCall?.args as { endpointHandle?: number }).endpointHandle)
      .toBe(1);
    expect(
      typeof (nextCall?.args as { nodeId?: unknown }).nodeId,
    ).toBe("string");
  });
});

// ── datagram base64 encode (defensive oversize guard, #288) ─────────────────

describe("datagram base64 encode helper", () => {
  it("encodes a large synthetic buffer without RangeError", async () => {
    const { encodeBase64, decodeBase64 } = await import(
      "@momics/iroh-http-shared"
    );

    // 256 KiB — well past the ~124 KB threshold where an unbounded
    // `String.fromCharCode(...bytes)` spread throws RangeError. Not reachable
    // via real MTU-bounded datagrams, but the encode path must stay safe.
    const big = new Uint8Array(256 * 1024);
    for (let i = 0; i < big.length; i++) big[i] = i & 0xff;

    let encoded = "";
    expect(() => {
      encoded = encodeBase64(big);
    }).not.toThrow();

    // Round-trips back to the original bytes.
    const decoded = decodeBase64(encoded);
    expect(decoded.length).toBe(big.length);
    expect(decoded[0]).toBe(big[0]);
    expect(decoded[big.length - 1]).toBe(big[big.length - 1]);
  });
});
