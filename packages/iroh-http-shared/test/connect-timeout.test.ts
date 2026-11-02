import type {
  FetchOptions,
  FfiResponse,
  IrohAdapter,
} from "../src/IrohAdapter.ts";
import { classifyError, IrohConnectError } from "../src/errors.ts";
import { makeFetch } from "../src/fetch.ts";

Deno.test("fetch maps connectTimeout to the bridge-only dial timeout", async () => {
  let observed: FetchOptions | undefined;
  const adapter = {
    rawFetch(
      _endpointHandle: number,
      _nodeAddr: string,
      _url: string,
      _method: string,
      _headers: [string, string][],
      _reqBodyHandle: bigint | null,
      _fetchToken: bigint,
      _directAddrs: string[] | null,
      options?: FetchOptions,
    ): Promise<FfiResponse> {
      observed = options;
      return Promise.resolve({
        status: 200,
        headers: [],
        bodyHandle: 0n,
        url: `httpi://${"b".repeat(52)}/`,
      });
    },
  } as unknown as IrohAdapter;

  await makeFetch(adapter, 1)(`httpi://${"b".repeat(52)}/`, {
    connectTimeout: 250,
    requestTimeout: 5_000,
  });

  if (observed?.connectTimeoutMs !== 250 || observed.timeoutMs !== 5_000) {
    throw new Error(`unexpected fetch options: ${JSON.stringify(observed)}`);
  }
});

Deno.test("CONNECT_TIMEOUT is a typed connection error distinct from TIMEOUT", () => {
  const connection = classifyError(
    '{"code":"CONNECT_TIMEOUT","message":"connection timed out"}',
  );
  const request = classifyError(
    '{"code":"TIMEOUT","message":"request timed out"}',
  );

  if (!(connection instanceof IrohConnectError)) {
    throw new Error("CONNECT_TIMEOUT should classify as IrohConnectError");
  }
  if (connection.code !== "CONNECT_TIMEOUT" || request.code !== "TIMEOUT") {
    throw new Error(
      "connection and request timeouts must retain distinct codes",
    );
  }
});

Deno.test("connection timeout releases an allocated request-body writer", async () => {
  let finished = 0;
  const adapter = {
    allocBodyWriter(): bigint {
      return 7n;
    },
    allocFetchToken(): Promise<bigint> {
      return Promise.resolve(9n);
    },
    cancelFetch(): void {},
    sendChunk(): Promise<void> {
      return Promise.resolve();
    },
    finishBody(handle: bigint): Promise<void> {
      if (handle !== 7n) throw new Error(`unexpected writer handle ${handle}`);
      finished += 1;
      return Promise.resolve();
    },
    rawFetch(): Promise<FfiResponse> {
      return Promise.reject(
        '{"code":"CONNECT_TIMEOUT","message":"connection timed out"}',
      );
    },
  } as unknown as IrohAdapter;

  try {
    await makeFetch(adapter, 1)(`httpi://${"b".repeat(52)}/`, {
      method: "POST",
      body: "upload",
      connectTimeout: 25,
    });
    throw new Error("fetch should reject");
  } catch (error) {
    if (
      !(error instanceof IrohConnectError) ||
      error.code !== "CONNECT_TIMEOUT"
    ) {
      throw error;
    }
  }

  if (finished !== 1) {
    throw new Error(`expected one writer release, got ${finished}`);
  }
});

Deno.test("abort during setup dispatches cancellation and releases native handles", async () => {
  const controller = new AbortController();
  let resolveWriter!: (handle: bigint) => void;
  let cancelled = false;
  let rawFetchCalls = 0;
  let finished = 0;
  const adapter = {
    allocBodyWriter(): Promise<bigint> {
      return new Promise((resolve) => {
        resolveWriter = resolve;
      });
    },
    allocFetchToken(): Promise<bigint> {
      return Promise.resolve(9n);
    },
    cancelFetch(token: bigint): void {
      if (token !== 9n) throw new Error(`unexpected fetch token ${token}`);
      cancelled = true;
    },
    finishBody(handle: bigint): Promise<void> {
      if (handle !== 7n) throw new Error(`unexpected writer handle ${handle}`);
      finished += 1;
      return Promise.resolve();
    },
    rawFetch(): Promise<FfiResponse> {
      rawFetchCalls += 1;
      if (!cancelled) throw new Error("fetch must observe setup cancellation");
      return Promise.reject('{"code":"CANCELLED","message":"aborted"}');
    },
  } as unknown as IrohAdapter;

  const result = makeFetch(adapter, 1)(`httpi://${"b".repeat(52)}/`, {
    method: "POST",
    body: "upload",
    signal: controller.signal,
  });
  controller.abort();
  resolveWriter(7n);

  try {
    await result;
    throw new Error("fetch should reject");
  } catch (error) {
    if (!(error instanceof Error) || error.name !== "AbortError") {
      throw error;
    }
  }
  await Promise.resolve();

  if (!cancelled || rawFetchCalls !== 1 || finished !== 1) {
    throw new Error(
      `unexpected cleanup: cancelled=${cancelled} rawFetchCalls=${rawFetchCalls} finished=${finished}`,
    );
  }
});
