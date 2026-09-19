import { normaliseRelayMode } from "../src/utils.ts";

const SYNTHETIC_TOKEN = "synthetic-relay-token-for-tests-only";

function assertEquals(actual: unknown, expected: unknown): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(
      `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
    );
  }
}

Deno.test("custom relay authentication is propagated separately from URLs", () => {
  assertEquals(
    normaliseRelayMode({
      urls: ["https://relay.invalid"],
      authToken: SYNTHETIC_TOKEN,
    }),
    {
      relayMode: "custom",
      relays: ["https://relay.invalid"],
      relayAuthToken: SYNTHETIC_TOKEN,
      disableNetworking: false,
    },
  );
});

Deno.test("relay authentication requires custom URLs without disclosing the token", () => {
  let error: unknown;
  try {
    normaliseRelayMode({ authToken: SYNTHETIC_TOKEN });
  } catch (cause) {
    error = cause;
  }
  if (!(error instanceof TypeError)) throw new Error("expected TypeError");
  if (!error.message.includes("requires a non-empty relay.urls list")) {
    throw new Error(`unexpected error: ${error.message}`);
  }
  assertEquals(error.message.includes(SYNTHETIC_TOKEN), false);
});
