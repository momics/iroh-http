/**
 * SHA-256 integrity checksums for the prebuilt native libraries published with
 * each release. Maps the platform-specific library filename (as produced by
 * `libName()` in `adapter.ts`) to its lowercase hex SHA-256 digest.
 *
 * **Populated at release time** by `scripts/gen-checksums.mts`, which hashes
 * the built artifacts before `deno publish`. The map is intentionally empty in
 * the source tree: local source builds have no stable digest and are loaded
 * with a warning, while *downloaded* binaries without a matching entry are
 * refused (see `verifyLib` in `adapter.ts`).
 *
 * Native FFI code runs entirely outside Deno's permission sandbox, so loading a
 * tampered or corrupted binary is arbitrary code execution in the host process.
 * Verifying the digest before `Deno.dlopen()` is the only thing standing
 * between a compromised release asset / cache / network path and RCE.
 */
export const NATIVE_CHECKSUMS: Record<string, string> = {
  // "libiroh_http_deno.darwin-aarch64.dylib": "<sha256-hex>",
  // ... filled in by scripts/gen-checksums.mts at release time.
};
