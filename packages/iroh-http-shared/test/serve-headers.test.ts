import type {
  FfiResponseHead,
  IrohAdapter,
  RequestPayload,
} from "../src/IrohAdapter.ts";
import { makeServe } from "../src/serve.ts";

type ServeCallback = (
  payload: RequestPayload,
) => Promise<FfiResponseHead>;

function assertEquals(
  actual: unknown,
  expected: unknown,
  message: string,
): void {
  if (actual !== expected) {
    throw new Error(
      `${message}: expected ${String(expected)}, got ${String(actual)}`,
    );
  }
}

Deno.test("serve exposes every inbound header when Request filters user-agent", async () => {
  const NativeRequest = globalThis.Request;

  class ChromiumRequest extends NativeRequest {
    constructor(input: RequestInfo | URL, init?: RequestInit) {
      const filtered = new Headers(init?.headers);
      filtered.delete("user-agent");
      super(input, { ...init, headers: filtered });
    }
  }

  let callback: ServeCallback | undefined;
  const adapter = {
    rawServe(
      _endpointHandle: number,
      _options: unknown,
      requestCallback: ServeCallback,
    ) {
      callback = requestCallback;
      return new Promise<void>(() => {});
    },
    nextChunk: () => Promise.resolve(null),
    sendChunk: () => Promise.resolve(),
    finishBody: () => Promise.resolve(),
  } as unknown as IrohAdapter;

  let observedHeaders: Headers | undefined;

  try {
    globalThis.Request = ChromiumRequest;
    const serve = makeServe(adapter, 1, new Promise<void>(() => {}));
    serve((request) => {
      observedHeaders = request.headers;
      return new Response(null, { status: 204 });
    });

    if (!callback) throw new Error("rawServe callback was not captured");
    await callback({
      method: "GET",
      url: "http://peer/resource",
      headers: [
        ["user-agent", "iroh-test/1.0"],
        ["x-iroh-test", "preserved"],
      ],
      remoteNodeId: "peer",
      reqHandle: 0n,
      reqBodyHandle: 0n,
      resBodyHandle: 0n,
    });
  } finally {
    globalThis.Request = NativeRequest;
  }

  if (!observedHeaders) throw new Error("serve handler was not called");
  assertEquals(
    observedHeaders.get("user-agent"),
    "iroh-test/1.0",
    "serve must expose the inbound user-agent",
  );
  assertEquals(
    observedHeaders.get("x-iroh-test"),
    "preserved",
    "serve must preserve ordinary inbound headers too",
  );
});
