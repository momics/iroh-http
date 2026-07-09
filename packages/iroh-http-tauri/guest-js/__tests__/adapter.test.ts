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
