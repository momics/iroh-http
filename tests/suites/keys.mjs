/**
 * Key / crypto tests — PublicKey, SecretKey, sign, verify, round-trips.
 *
 * Shared across all runtimes. The context receives { PublicKey, SecretKey }
 * classes along with the standard harness bindings.
 */

export function keyTests({ createNode, test, assert, assertEqual, PublicKey, SecretKey }) {
  test("SecretKey / PublicKey — sign and verify round-trip", async () => {
    const key = SecretKey.generate();
    const node = await createNode({ key });
    try {
      const sk = node.secretKey;
      const pk = node.publicKey;
      assert(sk.toBytes().length === 32, "SecretKey should be 32 bytes");
      assert(pk.bytes.length === 32, "PublicKey should be 32 bytes");

      const data = new TextEncoder().encode("test message");
      const sig = await sk.sign(data);
      assert(sig.length === 64, "Signature should be 64 bytes");

      const valid = await pk.verify(data, sig);
      assert(valid, "Signature should verify");

      const tampered = new Uint8Array(sig);
      tampered[0] ^= 0xff;
      const invalid = await pk.verify(data, tampered);
      assert(!invalid, "Tampered signature should not verify");
    } finally {
      await node.close();
    }
  });

  test("PublicKey.fromString — round-trip via node publicKey", async () => {
    const node = await createNode({ disableNetworking: true });
    try {
      const nodeIdStr = node.publicKey.toString();
      const pk2 = PublicKey.fromString(nodeIdStr);
      assert(node.publicKey.equals(pk2), "round-trip must produce equal keys");
    } finally {
      await node.close();
    }
  });

  test("SecretKey.fromBytes — round-trip via node secretKey", async () => {
    const key = SecretKey.generate();
    const node = await createNode({ key, disableNetworking: true });
    try {
      const sk = node.secretKey;
      const bytes = sk.toBytes();
      assertEqual(bytes.length, 32, "secretKey must be 32 bytes");
      const sk2 = SecretKey.fromBytes(bytes);
      assertEqual(
        Array.from(sk.toBytes()).join(","),
        Array.from(sk2.toBytes()).join(","),
        "round-trip must produce identical bytes",
      );
    } finally {
      await node.close();
    }
  });
}
