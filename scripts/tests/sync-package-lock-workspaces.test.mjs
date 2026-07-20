import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../..",
);
const fixtureRoot = mkdtempSync(join(tmpdir(), "iroh-http-package-lock-"));

try {
  mkdirSync(join(fixtureRoot, "packages/node"), { recursive: true });
  writeFileSync(
    join(fixtureRoot, "package.json"),
    JSON.stringify({ private: true, workspaces: ["packages/node"] }),
  );
  writeFileSync(
    join(fixtureRoot, "packages/node/package.json"),
    JSON.stringify({
      name: "@momics/iroh-http-node",
      version: "0.6.1",
      dependencies: { "third-party": "^1.0.0" },
      devDependencies: {},
      optionalDependencies: {
        "@momics/iroh-http-node-darwin-arm64": "0.6.1",
      },
    }),
  );
  writeFileSync(
    join(fixtureRoot, "package-lock.json"),
    JSON.stringify({
      name: "fixture",
      lockfileVersion: 3,
      packages: {
        "": { workspaces: ["packages/node"] },
        "node_modules/third-party": {
          version: "1.0.0",
          resolved: "https://registry.example/third-party-1.0.0.tgz",
          integrity: "sentinel-integrity",
        },
        "packages/node": {
          name: "@momics/iroh-http-node",
          version: "0.6.0",
          dependencies: { "third-party": "^1.0.0" },
          optionalDependencies: {
            "@momics/iroh-http-node-darwin-arm64": "0.6.0",
          },
        },
        "packages/node/node_modules/@momics/iroh-http-node-darwin-arm64": {
          version: "0.6.0",
          optional: true,
          cpu: ["arm64"],
          os: ["darwin"],
        },
      },
    }),
  );

  execFileSync(
    process.execPath,
    [
      join(repositoryRoot, "scripts/sync-package-lock-workspaces.mjs"),
      fixtureRoot,
    ],
    { stdio: "pipe" },
  );

  const lock = JSON.parse(
    readFileSync(join(fixtureRoot, "package-lock.json"), "utf8"),
  );
  assert.equal(lock.packages["packages/node"].version, "0.6.1");
  assert.equal(lock.packages["packages/node"].devDependencies, undefined);
  assert.equal(
    lock.packages["packages/node"].optionalDependencies[
      "@momics/iroh-http-node-darwin-arm64"
    ],
    "0.6.1",
  );
  assert.equal(
    lock.packages[
      "packages/node/node_modules/@momics/iroh-http-node-darwin-arm64"
    ].version,
    "0.6.1",
  );
  assert.deepEqual(lock.packages["node_modules/third-party"], {
    version: "1.0.0",
    resolved: "https://registry.example/third-party-1.0.0.tgz",
    integrity: "sentinel-integrity",
  });

  console.log("  4 passed, 0 failed");
} finally {
  rmSync(fixtureRoot, { recursive: true, force: true });
}
