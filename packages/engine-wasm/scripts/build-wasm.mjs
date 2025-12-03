import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const crateName = "engine-wasm";
const artifactName = crateName.replace(/-/g, "_");
const rustRoot = path.resolve(__dirname, "../../rust-core");
const targetWasm = path.join(
    rustRoot,
    "target",
    "wasm32-unknown-unknown",
    "release",
    `${artifactName}.wasm`,
);
const outDir = path.resolve(__dirname, "../pkg");

function run(cmd, args, cwd = process.cwd()) {
    const result = spawnSync(cmd, args, { cwd, stdio: "inherit" });
    if (result.status !== 0) {
        throw new Error(`Command failed: ${cmd} ${args.join(" ")}`);
    }
}

function ensureWasmBindgen() {
    const check = spawnSync("wasm-bindgen", ["--version"], { stdio: "pipe" });
    if (check.status !== 0) {
        throw new Error(
            "wasm-bindgen CLI is required. Install it with `cargo install wasm-bindgen-cli --version 0.2.106`.",
        );
    }
}

try {
    ensureWasmBindgen();

    run(
        "cargo",
        ["build", "--release", "--target", "wasm32-unknown-unknown", "-p", crateName],
        rustRoot,
    );

    if (!existsSync(targetWasm)) {
        throw new Error(`Built wasm not found at ${targetWasm}`);
    }

    rmSync(outDir, { recursive: true, force: true });
    mkdirSync(outDir, { recursive: true });

    run(
        "wasm-bindgen",
        ["--target", "web", "--typescript", "--out-dir", outDir, targetWasm],
        rustRoot,
    );

    console.log(`Wrote wasm bindings to ${outDir}`);
} catch (error) {
    console.error(error.message);
    process.exitCode = 1;
}
