import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const crateName = "engine-wasm";
const artifactName = crateName.replace(/-/g, "_");
const rustRoot = path.resolve(__dirname, "../../rust-core");

// --production フラグで本番ビルド（最大最適化）
const isProduction = process.argv.includes("--production");
// --skip-wasm-opt フラグでwasm-optをスキップ（デバッグ用）
const skipWasmOpt = process.argv.includes("--skip-wasm-opt");
const profile = isProduction ? "production" : "release";
const targetDir = isProduction ? "production" : "release";

const targetWasm = path.join(
    rustRoot,
    "target",
    "wasm32-unknown-unknown",
    targetDir,
    `${artifactName}.wasm`,
);
const outDir = path.resolve(__dirname, "../pkg");

function run(cmd, args, cwd = process.cwd()) {
    // Use shell string to properly inherit environment on Windows
    const fullCommand = `${cmd} ${args.join(" ")}`;

    // Ensure Rust environment variables are properly set
    const rustEnv = {
        ...process.env,
    };

    // Set default toolchain if not already set (Windows-specific issue workaround)
    if (!rustEnv.RUSTUP_TOOLCHAIN && process.platform === "win32") {
        rustEnv.RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-msvc";
    }

    // Try to find RUSTUP_HOME on Windows if not set (for non-standard installations)
    if (!rustEnv.RUSTUP_HOME && process.env.USERPROFILE) {
        const scoop = path.join(
            process.env.USERPROFILE,
            "scoop",
            "persist",
            "rustup-msvc",
            ".rustup",
        );
        const defaultLoc = path.join(process.env.USERPROFILE, ".rustup");
        if (existsSync(scoop)) {
            rustEnv.RUSTUP_HOME = scoop;
        } else if (existsSync(defaultLoc)) {
            rustEnv.RUSTUP_HOME = defaultLoc;
        }
    }

    const result = spawnSync(fullCommand, {
        cwd,
        stdio: "inherit",
        shell: true,
        env: rustEnv,
    });
    if (result.status !== 0) {
        throw new Error(`Command failed: ${fullCommand}`);
    }
}

function ensureWasmBindgen() {
    const check = spawnSync("wasm-bindgen --version", {
        stdio: "pipe",
        shell: true,
    });
    if (check.status !== 0) {
        throw new Error(
            "wasm-bindgen CLI is required. Install it with `cargo install wasm-bindgen-cli --version 0.2.106`.",
        );
    }
}

function runWasmOpt(wasmFile) {
    const beforeSize = statSync(wasmFile).size;

    // Rustが使用するWASM機能を有効化
    const enableFlags = [
        "--enable-bulk-memory",
        "--enable-nontrapping-float-to-int",
        "--enable-sign-ext",
        "--enable-mutable-globals",
    ];

    // productionでは-Oz（サイズ最適化）、それ以外は-O3（速度最適化）
    const optLevel = isProduction ? "-Oz" : "-O3";

    console.log(`Optimizing WASM with wasm-opt ${optLevel}...`);

    const result = spawnSync(
        "npx",
        ["wasm-opt", optLevel, ...enableFlags, wasmFile, "-o", wasmFile],
        {
            stdio: "inherit",
            shell: true,
        },
    );

    if (result.status !== 0) {
        console.warn(
            `wasm-opt failed with status ${result.status}, continuing without optimization`,
        );
        if (result.error) {
            console.warn(`Error details: ${result.error.message}`);
        }
        return;
    }

    const afterSize = statSync(wasmFile).size;
    const reduction = beforeSize - afterSize;
    const percent = ((reduction / beforeSize) * 100).toFixed(1);
    console.log(
        `wasm-opt: ${(beforeSize / 1024).toFixed(0)}KB -> ${(afterSize / 1024).toFixed(0)}KB (${percent}% reduction)`,
    );
}

try {
    ensureWasmBindgen();

    console.log(`Building ${crateName} with profile: ${profile}`);
    console.log(`Expected output: ${targetWasm}`);
    run(
        "cargo",
        ["build", "--profile", profile, "--target", "wasm32-unknown-unknown", "-p", crateName],
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

    // wasm-optで最適化（--skip-wasm-optでスキップ可能）
    const outputWasm = path.join(outDir, `${artifactName}_bg.wasm`);
    if (!skipWasmOpt && existsSync(outputWasm)) {
        runWasmOpt(outputWasm);
    }

    console.log(`Wrote wasm bindings to ${outDir}`);
} catch (error) {
    console.error(error.message);
    process.exitCode = 1;
}
