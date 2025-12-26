import path from "node:path";
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { visualizer } from "rollup-plugin-visualizer";
import { defineConfig } from "vite";

const rootDir = path.dirname(fileURLToPath(import.meta.url));

// ANALYZE=true でバンドル分析レポートを生成
const isAnalyze = process.env.ANALYZE === "true";

// https://vite.dev/config/
export default defineConfig(({ command }) => ({
    // GitHub Pages (https://sh11235.github.io/shogi/) 向けの base パス設定
    // - 開発時 (pnpm dev): "/" でローカルホストで動作
    // - ビルド時 (pnpm build): "/shogi/" で GitHub Pages のリポジトリページに対応
    // command === "build" による判定は Vite 公式の推奨方法で、環境変数の追加設定は不要
    base: command === "build" ? "/shogi/" : "/",
    server: {
        headers: {
            "Cross-Origin-Opener-Policy": "same-origin",
            "Cross-Origin-Embedder-Policy": "require-corp",
        },
    },
    preview: {
        headers: {
            "Cross-Origin-Opener-Policy": "same-origin",
            "Cross-Origin-Embedder-Policy": "require-corp",
        },
    },
    plugins: [
        tailwindcss(),
        react(),
        ...(isAnalyze
            ? [
                  visualizer({
                      filename: "dist/stats.html",
                      open: true,
                      gzipSize: true,
                      brotliSize: true,
                  }),
              ]
            : []),
    ],
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
        // React の重複インスタンスを防ぐ保険として dedupe を設定
        // バージョンが統一されていれば影響はないが、将来の安全性のため明示的に指定
        // ※ React バージョンを統一する場合: pnpm update react@X.X.X react-dom@X.X.X -r --filter "@shogi/*"
        dedupe: ["react", "react-dom"],
    },
}));
