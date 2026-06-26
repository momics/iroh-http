#!/usr/bin/env -S deno run --allow-run --allow-env --allow-read --allow-write
/**
 * Build the native library for the current platform and copy it into lib/.
 *
 * Usage:
 *   deno task build          # release
 *   deno task build:debug    # debug (faster compile)
 */

import { dirname, fromFileUrl, resolve } from "@std/path";
import { ensureDir } from "@std/fs";

const ROOT = resolve(dirname(fromFileUrl(import.meta.url)), "..");
const LIB_DIR = resolve(ROOT, "lib");
const RELEASE = Deno.args.includes("--debug") ? false : true;

// ── Platform lib name helpers ─────────────────────────────────────────────────

function ext(): string {
  switch (Deno.build.os) {
    case "darwin":
      return "dylib";
    case "windows":
      return "dll";
    default:
      return "so";
  }
}

function srcName(): string {
  // Rust's default output name (without the OS-specific prefix/suffix).
  // cargo adds "lib" prefix on Unix, no prefix on Windows.
  const base = "iroh_http_deno";
  if (Deno.build.os === "windows") return `${base}.${ext()}`;
  return `lib${base}.${ext()}`;
}

function destName(): string {
  return `libiroh_http_deno.${Deno.build.os}-${Deno.build.arch}.${ext()}`;
}

// ── Build ─────────────────────────────────────────────────────────────────────

const cargoArgs = ["cargo", "build", "--package", "iroh-http-deno"];
if (RELEASE) cargoArgs.push("--release");

console.log(
  `Building (${
    RELEASE ? "release" : "debug"
  }) for ${Deno.build.os}-${Deno.build.arch}…`,
);

const status = await new Deno.Command(cargoArgs[0], {
  args: cargoArgs.slice(1),
  cwd: resolve(ROOT, "../.."),
  stdin: "null",
  stdout: "inherit",
  stderr: "inherit",
}).output();

if (!status.success) {
  console.error("cargo build failed");
  Deno.exit(1);
}

// ── Copy to lib/ ─────────────────────────────────────────────────────────────

await ensureDir(LIB_DIR);

const profile = RELEASE ? "release" : "debug";
const srcPath = resolve(ROOT, "../..", "target", profile, srcName());
const destPath = resolve(LIB_DIR, destName());

await Deno.copyFile(srcPath, destPath);
console.log(`Wrote ${destPath}`);
