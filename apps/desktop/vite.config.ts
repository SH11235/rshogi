import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;
const rootDir = path.dirname(fileURLToPath(import.meta.url));

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],
  resolve: {
    alias: {
      "@shogi/app-core": path.resolve(rootDir, "../../packages/app-core/src"),
      "@shogi/design-system": path.resolve(rootDir, "../../packages/design-system/src"),
      "@shogi/ui": path.resolve(rootDir, "../../packages/ui/src"),
      "@shogi/engine-client": path.resolve(rootDir, "../../packages/engine-client/src"),
      "@shogi/engine-wasm": path.resolve(rootDir, "../../packages/engine-wasm/src"),
      "@shogi/engine-tauri": path.resolve(rootDir, "../../packages/engine-tauri/src"),
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
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
