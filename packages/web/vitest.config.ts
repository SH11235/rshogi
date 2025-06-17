import path from "node:path";
/// <reference types="vitest" />
import react from "@vitejs/plugin-react-swc";
import { defineConfig } from "vitest/config";

export default defineConfig({
    plugins: [react()],
    resolve: {
        alias: { "@": path.resolve(__dirname, "src") },
    },
    test: {
        environment: "jsdom", // DOM environment using jsdom for better stability
        include: ["src/**/*.test.{ts,tsx}", "src/__tests__/**/*.test.{ts,tsx}"],
        exclude: ["src/__tests__/utils/**"],
        globals: true, // expect, vi を毎回 import しなくて済む
        coverage: { provider: "v8" },
        setupFiles: ["./src/test/setup.ts"],
        typecheck: {
            tsconfig: "./tsconfig.test.json",
        },
    },
});
