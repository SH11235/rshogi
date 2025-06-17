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
        environment: "happy-dom",
        include: ["src/**/*.test.{ts,tsx}"],
        exclude: ["src/__tests__/**"],
        globals: true,
        coverage: { provider: "v8" },
        setupFiles: ["./src/test/setup.ts"],
        typecheck: {
            tsconfig: "./tsconfig.test.json",
        },
    },
});
