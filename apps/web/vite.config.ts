import path from "node:path";
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const rootDir = path.dirname(fileURLToPath(import.meta.url));

// https://vite.dev/config/
export default defineConfig({
    plugins: [react()],
    resolve: {
        alias: [
            {
                find: /^@shogi\/app-core$/,
                replacement: path.resolve(rootDir, "../../packages/app-core/src/index.web.ts"),
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
                find: "@shogi/engine-wasm",
                replacement: path.resolve(rootDir, "../../packages/engine-wasm/src"),
            },
            {
                find: "@shogi/engine-tauri",
                replacement: path.resolve(rootDir, "../../packages/engine-tauri/src"),
            },
        ],
    },
});
