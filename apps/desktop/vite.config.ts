import path from "node:path";
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const host = process.env.TAURI_DEV_HOST;
const rootDir = path.dirname(fileURLToPath(import.meta.url));

// https://vite.dev/config/
export default defineConfig(async () => ({
    plugins: [react()],
    resolve: {
        alias: [
            {
                find: "@shogi/app-core",
                replacement: path.resolve(rootDir, "../../packages/app-core/src"),
            },
            {
                find: "@shogi/design-system",
                replacement: path.resolve(rootDir, "../../packages/design-system/src"),
            },
            { find: "@shogi/ui", replacement: path.resolve(rootDir, "../../packages/ui/src") },
            {
                find: "@shogi/engine-client",
                replacement: path.resolve(rootDir, "../../packages/engine-client/src"),
            },
            {
                find: "@shogi/engine-tauri",
                replacement: path.resolve(rootDir, "../../packages/engine-tauri/src"),
            },
        ],
        // React の重複インスタンスを防ぐ保険として dedupe を設定
        // バージョンが統一されていれば影響はないが、将来の安全性のため明示的に指定
        // ※ React バージョンを統一する場合: pnpm update react@X.X.X react-dom@X.X.X -r --filter "@shogi/*"
        dedupe: ["react", "react-dom"],
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
