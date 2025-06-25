import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react-swc";
import { defineConfig } from "vite";
import topLevelAwait from "vite-plugin-top-level-await";
import wasm from "vite-plugin-wasm";

// https://vite.dev/config/
export default defineConfig({
    plugins: [wasm(), topLevelAwait(), react(), tailwindcss()],
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
});
