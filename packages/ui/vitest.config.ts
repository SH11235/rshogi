import { defineConfig } from "vitest/config";

export default defineConfig({
    test: {
        environment: "happy-dom",
        globals: true,
    },
    resolve: {
        alias: {
            "@shogi/engine-wasm/pkg/engine_wasm.js": new URL(
                "./src/__mocks__/engine-wasm.ts",
                import.meta.url,
            ).pathname,
        },
    },
});
