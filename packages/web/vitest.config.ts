import path from "node:path";
/// <reference types="vitest" />
import react from "@vitejs/plugin-react-swc";
import { defineConfig } from "vite";

export default defineConfig({
    plugins: [react()],
    resolve: {
        alias: { "@": path.resolve(__dirname, "src") },
    },
    test: {
        environment: "happy-dom", // DOM 必要な場合／Node だけなら 'node'
        include: ["src/**/*.test.{ts,tsx}"],
        globals: true, // expect, vi を毎回 import しなくて済む
        coverage: { provider: "v8" },
        setupFiles: ["./src/test/setup.ts"],
    },
});
