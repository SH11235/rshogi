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
