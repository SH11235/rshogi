import { defineConfig } from "vitest/config";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));

// 全中断点を回すため smoke より桁違いに遅い。smoke とは別 script で実行する。
export default defineConfig({
  test: {
    dir: here,
    globalSetup: [`${here}/../miniflare_smoke/global-setup.ts`],
    include: ["*.test.ts"],
    testTimeout: 1_800_000,
    hookTimeout: 60_000,
    fileParallelism: false,
    pool: "forks",
  },
});
