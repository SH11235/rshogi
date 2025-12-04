import path from "node:path";
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const rootDir = path.dirname(fileURLToPath(import.meta.url));

// https://vite.dev/config/
export default defineConfig({
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
});
