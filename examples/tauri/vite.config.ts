import path from "path";
import { defineConfig } from "vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  resolve: {
    alias: {
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

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. target modern browsers that support top-level await
  build: {
    target: "esnext",
  },
  // 3. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
        protocol: "ws",
        host,
        port: 1421,
      }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
