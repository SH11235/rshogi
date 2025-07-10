import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react-swc";
import { defineConfig } from "vite";
import topLevelAwait from "vite-plugin-top-level-await";
import wasm from "vite-plugin-wasm";

// https://vite.dev/config/
export default defineConfig({
    plugins: [
        wasm(),
        topLevelAwait(),
        react(),
        tailwindcss(),
        // .gz/.binzファイルの自動解凍を無効化するプラグイン
        {
            name: "disable-gz-decompression",
            configureServer(server) {
                server.middlewares.use((req, res, next) => {
                    if (req.url?.endsWith(".bin.gz") || req.url?.endsWith(".binz")) {
                        const originalSetHeader = res.setHeader;
                        res.setHeader = function (name, value) {
                            if (name.toLowerCase() === "content-encoding") {
                                return res;
                            }
                            return originalSetHeader.call(this, name, value);
                        };
                        // MIMEタイプを明示的に設定
                        res.setHeader("Content-Type", "application/octet-stream");
                    }
                    next();
                });
            },
        },
    ],
    resolve: {
        alias: {
            "@": path.resolve(__dirname, "./src"),
        },
    },
    base: process.env.NODE_ENV === "production" ? "/shogi/" : "/",
    build: {
        outDir: "dist",
        assetsDir: "assets",
        sourcemap: false,
    },
    worker: {
        plugins: () => [wasm(), topLevelAwait()],
    },
    assetsInclude: ["**/*.gz"],
});
