import type { FfiResponse, IrohAdapter } from "../src/IrohAdapter.ts";
import { makeFetch } from "../src/fetch.ts";
import { IrohNode } from "../src/IrohNode.ts";
import type { RawSessionFns } from "../src/session.ts";

function assertEquals(actual: unknown, expected: unknown): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(
      `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
    );
  }
}

Deno.test("fetch carries an explicit discovered relay URL in the node ticket", async () => {
  let remoteNodeAddr: string | undefined;
  let directAddrs: string[] | null | undefined;
  const adapter = {
    rawFetch(
      _endpointHandle: number,
      nodeAddr: string,
      _url: string,
      _method: string,
      _headers: [string, string][],
      _reqBodyHandle: bigint | null,
      _fetchToken: bigint,
      addrs: string[] | null,
    ): Promise<FfiResponse> {
      remoteNodeAddr = nodeAddr;
      directAddrs = addrs;
      return Promise.resolve({
        status: 200,
        headers: [],
        bodyHandle: 0n,
        inlineBody: "",
        url: "httpi://peer.example/health",
      });
    },
  } as unknown as IrohAdapter;

  const fetch = makeFetch(adapter, 7);
  await fetch(`httpi://${"b".repeat(52)}/health`, {
    directAddrs: ["192.0.2.4:4433"],
    relayUrl: "https://relay.example",
  });

  assertEquals(JSON.parse(remoteNodeAddr!), {
    id: "b".repeat(52),
    addrs: ["https://relay.example"],
  });
  assertEquals(directAddrs, ["192.0.2.4:4433"]);
});

Deno.test("dial carries an explicit discovered relay URL in the node ticket", async () => {
  let remoteNodeAddr: string | undefined;
  let directAddrs: string[] | null | undefined;
  const rawSession = {
    connect(
      _endpointHandle: number,
      nodeAddr: string,
      addrs: string[] | null,
    ): Promise<bigint> {
      remoteNodeAddr = nodeAddr;
      directAddrs = addrs;
      return Promise.resolve(9n);
    },
    closed(): Promise<{ closeCode: number; reason: string }> {
      return new Promise(() => {});
    },
  } as unknown as RawSessionFns;
  const adapter = {
    sessionFns: rawSession,
    startTransportEvents() {},
  } as unknown as IrohAdapter;
  const node = IrohNode._create(
    adapter,
    { endpointHandle: 7, nodeId: "a".repeat(52) },
    undefined,
    new Promise(() => {}),
  );

  await node.dial("b".repeat(52), {
    directAddrs: ["192.0.2.5:4433"],
    relayUrl: "https://relay.example",
  });

  assertEquals(JSON.parse(remoteNodeAddr!), {
    id: "b".repeat(52),
    addrs: ["https://relay.example"],
  });
  assertEquals(directAddrs, ["192.0.2.5:4433"]);
});
