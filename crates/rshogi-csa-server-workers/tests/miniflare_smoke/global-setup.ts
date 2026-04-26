import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";

const WORKER_ROOT = resolve(import.meta.dirname, "../..");
const SHIM_PATH = resolve(WORKER_ROOT, "build/worker/shim.mjs");

export default async function setup(): Promise<void> {
  if (process.env.MINIFLARE_SMOKE_SKIP_BUILD === "1" && existsSync(SHIM_PATH)) {
    return;
  }
  await runWorkerBuild();
  if (!existsSync(SHIM_PATH)) {
    throw new Error(`worker-build did not produce ${SHIM_PATH}`);
  }
}

function runWorkerBuild(): Promise<void> {
  return new Promise((resolveBuild, reject) => {
    // `RUSTFLAGS` を空文字で上書きする理由: 開発者ローカルや CI 上位 env で
    // `-C target-cpu=x86-64-v2` 等の host 用 flag が立っていると、wasm32 では
    // "not a recognized processor" 警告が大量出力されたり、`cargo` が host
    // sysroot キャッシュと混在して codegen が壊れることがある。Workers の
    // wasm32 ビルドは `Swatinem/rust-cache` の `shared-key: wasm32` 経路と
    // 揃える必要があり、そこも RUSTFLAGS="" で固定済 (workers-smoke.yml /
    // rust-ci.yml `wasm-build` job / deploy-workers.yml の 3 箇所と同契約)。
    const child = spawn("worker-build", ["--release"], {
      cwd: WORKER_ROOT,
      stdio: "inherit",
      env: { ...process.env, RUSTFLAGS: "" },
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) resolveBuild();
      else reject(new Error(`worker-build exited with code ${code}`));
    });
  });
}
