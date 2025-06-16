import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react-swc";
import { defineConfig } from "vite";

// https://vite.dev/config/
export default defineConfig({
    plugins: [react(), tailwindcss()],
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
