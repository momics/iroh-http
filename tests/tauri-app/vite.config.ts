import path from "path";
import { defineConfig } from "vite";

// Resolve the repo root so Vite is allowed to bundle the shared runner and
// suites that live outside this app's directory (../../runners, ../../suites).
const repoRoot = path.resolve(__dirname, "../..");

// https://vite.dev/config/
export default defineConfig({
  resolve: {
    alias: {
      // Shared sources are outside this test app, so normal importer-relative
      // resolution cannot see the app-local dependency installed in CI.
      "@tauri-apps/api": path.resolve(
        __dirname,
        "node_modules/@tauri-apps/api",
      ),
      "@momics/iroh-http-tauri": path.resolve(
        __dirname,
        "../../packages/iroh-http-tauri/guest-js/index.ts",
      ),
      // More specific subpath alias must come first — Rollup matches aliases in
      // order, so the bare alias below would otherwise rewrite this to
      // `src/index.ts/adapter`.
      "@momics/iroh-http-shared/adapter": path.resolve(
        __dirname,
        "../../packages/iroh-http-shared/src/adapter.ts",
      ),
      "@momics/iroh-http-shared": path.resolve(
        __dirname,
        "../../packages/iroh-http-shared/src/index.ts",
      ),
    },
  },

  // Prevent Vite from obscuring rust errors.
  clearScreen: false,
  // Target modern browsers that support top-level await.
  build: {
    target: "esnext",
  },
  server: {
    port: 1420,
    strictPort: true,
    // Allow serving the shared runner/suites that live above this app.
    fs: {
      allow: [repoRoot],
    },
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
});
