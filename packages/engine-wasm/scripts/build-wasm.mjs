import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync } from "node:fs";
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

try {
    ensureWasmBindgen();

    console.log(`Building with profile: ${profile}`);
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

    console.log(`Wrote wasm bindings to ${outDir}`);
} catch (error) {
    console.error(error.message);
    process.exitCode = 1;
}
