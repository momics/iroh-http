#!/usr/bin/env -S deno run --allow-run --allow-env --allow-read --allow-write
/**
 * Cross-compile iroh-http-deno for all five supported platforms.
 *
 * Prerequisites (macOS host):
 *   rustup target add \
 *     aarch64-apple-darwin x86_64-apple-darwin \
 *     x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
 *     x86_64-pc-windows-gnu
 *   cargo install cargo-zigbuild
 *   brew install zig mingw-w64
 */

import { dirname, fromFileUrl, resolve } from "@std/path";
import { ensureDir } from "@std/fs";

const ROOT = resolve(dirname(fromFileUrl(import.meta.url)), "..");
const LIB_DIR = resolve(ROOT, "lib");
const WORKSPACE_ROOT = resolve(ROOT, "../..");

interface Platform {
  target: string;
  os: string;
  arch: string;
  ext: string;
  /** Use cargo-zigbuild instead of plain cargo (Linux targets). */
  zig: boolean;
}

const PLATFORMS: Platform[] = [
  {
    target: "aarch64-apple-darwin",
    os: "darwin",
    arch: "aarch64",
    ext: "dylib",
    zig: false,
  },
  {
    target: "x86_64-apple-darwin",
    os: "darwin",
    arch: "x86_64",
    ext: "dylib",
    zig: false,
  },
  {
    target: "x86_64-unknown-linux-gnu",
    os: "linux",
    arch: "x86_64",
    ext: "so",
    zig: true,
  },
  {
    target: "aarch64-unknown-linux-gnu",
    os: "linux",
    arch: "aarch64",
    ext: "so",
    zig: true,
  },
  {
    target: "x86_64-pc-windows-gnu",
    os: "windows",
    arch: "x86_64",
    ext: "dll",
    zig: false,
  },
];

// ── Helpers ───────────────────────────────────────────────────────────────────

function srcName(p: Platform): string {
  const base = "iroh_http_deno";
  if (p.os === "windows") return `${base}.${p.ext}`;
  return `lib${base}.${p.ext}`;
}

function destName(p: Platform): string {
  return `libiroh_http_deno.${p.os}-${p.arch}.${p.ext}`;
}

async function run(cmd: string, args: string[]): Promise<void> {
  const status = await new Deno.Command(cmd, {
    args,
    cwd: WORKSPACE_ROOT,
    stdin: "null",
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  if (!status.success) {
    throw new Error(`Command failed: ${cmd} ${args.join(" ")}`);
  }
}

// ── Build all platforms ───────────────────────────────────────────────────────

await ensureDir(LIB_DIR);

let failed = 0;

for (const p of PLATFORMS) {
  console.log(`\n── ${p.target} ─────────────────────────────────`);
  try {
    if (p.zig) {
      await run("cargo", [
        "zigbuild",
        "--package",
        "iroh-http-deno",
        "--release",
        "--target",
        p.target,
      ]);
    } else {
      await run("cargo", [
        "build",
        "--package",
        "iroh-http-deno",
        "--release",
        "--target",
        p.target,
      ]);
    }

    const srcPath = resolve(
      WORKSPACE_ROOT,
      "target",
      p.target,
      "release",
      srcName(p),
    );
    const destPath = resolve(LIB_DIR, destName(p));
    await Deno.copyFile(srcPath, destPath);
    console.log(`  → ${destPath}`);
  } catch (err) {
    console.error(`  FAILED: ${(err as Error).message}`);
    failed++;
  }
}

if (failed > 0) {
  console.error(`\n${failed} platform(s) failed.`);
  Deno.exit(1);
} else {
  console.log(`\nAll platforms built successfully.`);
}
