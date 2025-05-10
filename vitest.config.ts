/// <reference types="vitest" />
import { defineConfig } from "vite";
import path from "node:path";

export default defineConfig({
    resolve: {
        alias: { "@": path.resolve(__dirname, "src") },
    },
    test: {
        environment: "happy-dom", // DOM 必要な場合／Node だけなら 'node'
        include: ["src/**/*.test.{ts,tsx}"],
        globals: true, // expect, vi を毎回 import しなくて済む
        coverage: { provider: "v8" },
    },
});
