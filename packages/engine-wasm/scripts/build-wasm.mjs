import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const crateName = "engine-wasm";
const artifactName = crateName.replace(/-/g, "_");
const rustRoot = path.resolve(__dirname, "../../rust-core");
const nightlyToolchain = process.env.RUST_NIGHTLY_TOOLCHAIN ?? "nightly-2025-12-25";
const pnpmCmd = process.platform === "win32" ? "pnpm.cmd" : "pnpm";

// --production フラグで本番ビルド（最大最適化）
const isProduction = process.argv.includes("--production");
// --skip-wasm-opt フラグでwasm-optをスキップ（デバッグ用）
const skipWasmOpt = process.argv.includes("--skip-wasm-opt");
const profile = isProduction ? "production" : "release";
const targetDir = isProduction ? "production" : "release";

const outDir = path.resolve(__dirname, "../pkg");
const threadedOutDir = path.resolve(__dirname, "../pkg-threaded");
const threadedTarget = path.resolve(rustRoot, "targets", "wasm32-unknown-unknown.json");

function run(cmd, args, cwd = process.cwd(), extraEnv = {}) {
    const fullCommand = `${cmd} ${args.join(" ")}`;

    // Ensure Rust environment variables are properly set
    const rustEnv = {
        ...process.env,
        ...extraEnv,
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

    const result = spawnSync(cmd, args, {
        cwd,
        stdio: "inherit",
        env: rustEnv,
    });
    if (result.status !== 0) {
        throw new Error(`Command failed: ${fullCommand}`);
    }
}

function ensureWasmBindgen() {
    const check = spawnSync("wasm-bindgen", ["--version"], {
        stdio: "pipe",
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
        "--enable-simd",
        "--enable-bulk-memory",
        "--enable-nontrapping-float-to-int",
        "--enable-sign-ext",
        "--enable-mutable-globals",
        "--enable-threads",
    ];

    // productionでは-Oz（サイズ最適化）、それ以外は-O3（速度最適化）
    const optLevel = isProduction ? "-Oz" : "-O3";

    console.log(`Optimizing WASM with wasm-opt ${optLevel}...`);

    const result = spawnSync(
        pnpmCmd,
        ["exec", "wasm-opt", optLevel, ...enableFlags, wasmFile, "-o", wasmFile],
        { stdio: "inherit" },
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

function buildWasm({
    label,
    cargoArgs,
    outDir: outputDir,
    rustflags,
    toolchain,
    cargoZArgs,
    emitThreadWorker,
    target,
    enableWasmOpt = true,
}) {
    const targetArg = target ?? "wasm32-unknown-unknown";
    const targetName = targetArg.endsWith(".json") ? path.basename(targetArg, ".json") : targetArg;
    const targetWasm = path.join(rustRoot, "target", targetName, targetDir, `${artifactName}.wasm`);

    console.log(`Building ${crateName}${label ? ` (${label})` : ""} with profile: ${profile}`);
    console.log(`Expected output: ${targetWasm}`);

    const extraEnv = rustflags
        ? { RUSTFLAGS: [process.env.RUSTFLAGS, rustflags].filter(Boolean).join(" ") }
        : {};

    const args = [
        ...(toolchain ? [`+${toolchain}`] : []),
        "build",
        "--profile",
        profile,
        "--target",
        targetArg,
        "-p",
        crateName,
        ...cargoArgs,
        ...(cargoZArgs ?? []),
    ];

    run("cargo", args, rustRoot, extraEnv);

    if (!existsSync(targetWasm)) {
        throw new Error(`Built wasm not found at ${targetWasm}`);
    }

    rmSync(outputDir, { recursive: true, force: true });
    mkdirSync(outputDir, { recursive: true });

    run(
        "wasm-bindgen",
        ["--target", "web", "--typescript", "--out-dir", outputDir, targetWasm],
        rustRoot,
    );

    // wasm-optで最適化（--skip-wasm-optでスキップ可能）
    const outputWasm = path.join(outputDir, `${artifactName}_bg.wasm`);
    if (enableWasmOpt && !skipWasmOpt && existsSync(outputWasm)) {
        runWasmOpt(outputWasm);
    }

    if (emitThreadWorker) {
        const workerPath = path.join(outputDir, `${artifactName}_worker.js`);
        const workerSource = [
            `import { initSync } from "./${artifactName}.js";`,
            "",
            "self.onmessage = (event) => {",
            "    const { module, memory, thread_stack_size: threadStackSize } = event.data ?? {};",
            "    if (!module || !memory) return;",
            "    initSync({ module, memory, thread_stack_size: threadStackSize });",
            '    self.postMessage({ type: "ready" });',
            "};",
            "",
        ].join("\n");
        writeFileSync(workerPath, workerSource, "utf8");
    }

    console.log(`Wrote wasm bindings to ${outputDir}`);
}

function verifyThreadedOutput(outputDir) {
    const jsPath = path.join(outputDir, `${artifactName}.js`);
    const workerPath = path.join(outputDir, `${artifactName}_worker.js`);

    if (!existsSync(workerPath)) {
        throw new Error(`Threaded worker script not found at ${workerPath}`);
    }

    if (!existsSync(jsPath)) {
        throw new Error(`Threaded JS output not found at ${jsPath}`);
    }

    const jsSource = readFileSync(jsPath, "utf8");
    if (!jsSource.includes("initThreadPool")) {
        throw new Error("initThreadPool export missing in threaded wasm output");
    }
}

try {
    ensureWasmBindgen();

    buildWasm({ label: "single", cargoArgs: [], outDir });

    buildWasm({
        label: "threaded",
        cargoArgs: ["--features", "wasm-threads"],
        outDir: threadedOutDir,
        rustflags:
            "-Z unstable-options -C panic=immediate-abort -C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=2147483648 -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_base -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align",
        toolchain: nightlyToolchain,
        cargoZArgs: ["-Z", "build-std=core,alloc,std"],
        emitThreadWorker: true,
        target: threadedTarget,
        enableWasmOpt: true,
    });

    verifyThreadedOutput(threadedOutDir);
} catch (error) {
    console.error(error.message);
    process.exitCode = 1;
}
