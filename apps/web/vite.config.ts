import path from "node:path";
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const rootDir = path.dirname(fileURLToPath(import.meta.url));

// https://vite.dev/config/
export default defineConfig(({ command }) => ({
    // GitHub Pages (https://sh11235.github.io/shogi/) 向けの base パス設定
    // - 開発時 (pnpm dev): "/" でローカルホストで動作
    // - ビルド時 (pnpm build): "/shogi/" で GitHub Pages のリポジトリページに対応
    // command === "build" による判定は Vite 公式の推奨方法で、環境変数の追加設定は不要
    base: command === "build" ? "/shogi/" : "/",
    plugins: [react()],
    resolve: {
        alias: [
            {
                find: /^@shogi\/app-core$/,
                replacement: path.resolve(rootDir, "../../packages/app-core/src"),
            },
            {
                find: /^@shogi\/design-system$/,
                replacement: path.resolve(rootDir, "../../packages/design-system/src"),
            },
            { find: /^@shogi\/ui$/, replacement: path.resolve(rootDir, "../../packages/ui/src") },
            {
                find: /^@shogi\/engine-client$/,
                replacement: path.resolve(rootDir, "../../packages/engine-client/src"),
            },
            {
                find: /^@shogi\/engine-wasm$/,
                replacement: path.resolve(rootDir, "../../packages/engine-wasm/src"),
            },
        ],
    },
}));
