#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(
  process.argv[2] ?? new URL("..", import.meta.url).pathname,
);
const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));

const rootManifest = readJson(resolve(root, "package.json"));
const lockPath = resolve(root, "package-lock.json");
const lock = readJson(lockPath);

if (lock.lockfileVersion !== 3 || typeof lock.packages !== "object") {
  throw new Error("Expected an npm lockfileVersion 3 package-lock.json");
}

for (const workspacePath of rootManifest.workspaces ?? []) {
  if (workspacePath.includes("*")) {
    throw new Error(`Workspace globs are not supported: ${workspacePath}`);
  }

  const manifest = readJson(resolve(root, workspacePath, "package.json"));
  const lockedWorkspace = lock.packages[workspacePath];
  if (!lockedWorkspace) {
    throw new Error(
      `package-lock.json has no entry for workspace ${workspacePath}`,
    );
  }

  lockedWorkspace.version = manifest.version;
  for (
    const field of [
      "dependencies",
      "devDependencies",
      "optionalDependencies",
      "peerDependencies",
    ]
  ) {
    if (
      manifest[field] === undefined ||
      Object.keys(manifest[field]).length === 0
    ) {
      delete lockedWorkspace[field];
    } else {
      lockedWorkspace[field] = manifest[field];
    }
  }

  for (
    const [name, version] of Object.entries(manifest.optionalDependencies ?? {})
  ) {
    const nestedPath = `${workspacePath}/node_modules/${name}`;
    const nestedPackage = lock.packages[nestedPath];
    if (nestedPackage && /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
      nestedPackage.version = version;
    }
  }
}

writeFileSync(lockPath, `${JSON.stringify(lock, null, 2)}\n`);
