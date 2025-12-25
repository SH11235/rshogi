import { defineConfig } from "vitest/config";

export default defineConfig({
    test: {
        environment: "node",
        globals: true,
        exclude: ["dist/**"],
    },
    resolve: {
        alias: {
            "../pkg/engine_wasm.js": new URL("./src/__mocks__/engine-wasm-pkg.ts", import.meta.url)
                .pathname,
        },
    },
});
