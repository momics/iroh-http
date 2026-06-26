#!/usr/bin/env node
/**
 * Cross-compile iroh-http-node for all supported platforms.
 *
 * Prerequisites (macOS host):
 *   rustup target add \
 *     aarch64-apple-darwin x86_64-apple-darwin \
 *     x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
 *     x86_64-pc-windows-msvc
 *   cargo install cargo-zigbuild cargo-xwin
 *   brew install zig
 */
import { execSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "..");

// Ensure brew LLVM tools (llvm-lib, clang-cl) are on PATH for cargo-xwin.
const LLVM_BIN = "/opt/homebrew/opt/llvm/bin";
const PATH = process.env.PATH?.includes(LLVM_BIN)
  ? process.env.PATH
  : `${LLVM_BIN}:${process.env.PATH ?? ""}`;
const env = { ...process.env, PATH };

const TARGETS = [
  { target: "aarch64-apple-darwin", zig: false },
  { target: "x86_64-apple-darwin", zig: false },
  { target: "x86_64-unknown-linux-gnu", zig: true },
  { target: "aarch64-unknown-linux-gnu", zig: true },
  { target: "x86_64-pc-windows-msvc", zig: false },
];

let failed = 0;

for (const { target, zig } of TARGETS) {
  console.log(`\n── ${target} ──`);
  const zigFlag = zig ? " --zig" : "";
  const cmd =
    `npx napi build --platform --release --target ${target}${zigFlag}`;
  try {
    execSync(cmd, { cwd: PKG_DIR, stdio: "inherit", env });
    console.log(`  ✓ ${target}`);
  } catch {
    console.error(`  ✗ ${target} (build failed)`);
    failed++;
  }
}

// Compile TypeScript wrapper
console.log("\n── TypeScript ──");
try {
  execSync("npx tsc", { cwd: PKG_DIR, stdio: "inherit" });
  console.log("  ✓ lib.ts → lib.js + lib.d.ts");
} catch {
  console.error("  ✗ tsc failed");
  failed++;
}

// List results
const nodes = readdirSync(PKG_DIR).filter((f) => f.endsWith(".node"));
console.log(`\nBuilt ${nodes.length} binaries:`);
for (const f of nodes) console.log(`  ${f}`);

if (failed > 0) {
  console.error(`\n${failed} target(s) failed`);
  process.exit(1);
}
