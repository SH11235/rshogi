import { spawnSync } from "node:child_process";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const features = ["halfkp", "halfka_split", "halfka_merged", "halfka_hm_split", "halfka_hm_merged"];
const usage = "Usage: node scripts/check_fixed_wasm.mjs [--ft halfkp|halfka_split|halfka_merged|halfka_hm_split|halfka_hm_merged]";
const args = process.argv.slice(2);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const targetDir = process.env.CARGO_TARGET_DIR
    ? resolve(process.cwd(), process.env.CARGO_TARGET_DIR)
    : resolve(root, "target", "fixed-edition-wasm");
const target = "wasm32-wasip1";
const wasm = resolve(targetDir, target, "debug", "rshogi-usi.wasm");
const runner = resolve(root, "scripts", "run_wasi.mjs");

function run(command, arguments_, options) {
    const result = spawnSync(command, arguments_, {
        cwd: root,
        windowsHide: true,
        ...options,
    });
    if (result.error || result.status !== 0) {
        if (result.stdout) process.stdout.write(result.stdout);
        if (result.stderr) process.stderr.write(result.stderr);
        if (result.error) throw result.error;
        throw new Error(`${command} failed: status=${result.status}, signal=${result.signal}`);
    }
    return result;
}

function main() {
    if (args.length === 1 && args[0] === "--help") {
        console.log(usage);
        return;
    }
    let selected = features;
    if (args.length !== 0) {
        if (args.length !== 2 || args[0] !== "--ft" || !features.includes(args[1])) {
            throw new Error(usage);
        }
        selected = [args[1]];
    }
    const env = { ...process.env };
    // host の CPU 指定と encoded flags の優先順位で WASM 設定が変わるのを防ぐ。
    for (const key of Object.keys(env)) {
        if (["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS"].includes(key.toUpperCase())
            || /^CARGO_TARGET_.*_RUSTFLAGS$/i.test(key)) delete env[key];
    }
    for (const ft of selected) {
        const edition = `edition-layerstacks-${ft}-1536x16x32-none`;
        for (const simd of [false, true]) {
            const label = `${edition} ${simd ? "+simd128" : "-simd128"}`;
            console.log(`Build: ${label}`);
            run("cargo", ["build", "--locked", "-p", "rshogi-usi", "--bin", "rshogi-usi",
                "--target", target, "--target-dir", targetDir, "--no-default-features",
                "--features", edition, "--jobs", "2"], {
                env: { ...env, RUSTFLAGS: `-C target-feature=${simd ? "+" : "-"}simd128` },
                stdio: "inherit",
                timeout: 30 * 60 * 1000,
            });
            const result = run(process.execPath, [runner, wasm], {
                env,
                input: "usi\nsetoption name MaterialLevel value 1\nsetoption name USI_Hash value 1\nisready\nquit\n",
                encoding: "utf8",
                timeout: 60 * 1000,
                maxBuffer: 4 * 1024 * 1024,
            });
            process.stdout.write(result.stdout);
            process.stderr.write(result.stderr);
            const lines = new Set(result.stdout.split(/\r?\n/).map((line) => line.trim()));
            if (!lines.has("usiok") || !lines.has("readyok")) {
                throw new Error(`${label}: missing usiok or readyok`);
            }
            console.log(`PASS: ${label}`);
        }
    }
}

try {
    main();
} catch (error) {
    console.error(error.message);
    process.exitCode = 1;
}
