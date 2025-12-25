import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const crateName = "engine-wasm";
const artifactName = crateName.replace(/-/g, "_");
const rustRoot = path.resolve(__dirname, "../../rust-core");

// --production フラグで本番ビルド（最大最適化）
const isProduction = process.argv.includes("--production");
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
const threadedOutDir = path.resolve(__dirname, "../pkg-threaded");

function run(cmd, args, cwd = process.cwd(), extraEnv = {}) {
    // Use shell string to properly inherit environment on Windows
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

function buildWasm({
    label,
    cargoArgs,
    outDir: outputDir,
    rustflags,
    toolchain,
    cargoZArgs,
    emitThreadWorker,
}) {
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
        "wasm32-unknown-unknown",
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
            "-C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=2147483648 -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_base -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align",
        toolchain: "nightly",
        cargoZArgs: ["-Z", "build-std=std,panic_abort"],
        emitThreadWorker: true,
    });

    verifyThreadedOutput(threadedOutDir);
} catch (error) {
    console.error(error.message);
    process.exitCode = 1;
}
