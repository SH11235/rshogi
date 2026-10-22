// WASI 向けにビルドした wasm を Node.js (20 以降) で動かす。wasm の速さを V8 (Chromium と同じエンジン) で測る用。
// usage: node scripts/run_wasi.mjs <file.wasm> [args...]
// wasm から見えるのはカレントディレクトリだけ (読み書きできる)。ファイルはカレントディレクトリからの
// 相対パスで渡す ("../" や絶対パスは開けない)。Windows の "\" 区切りは "/" に直して渡す。
import { readFile } from "node:fs/promises";
import { WASI } from "node:wasi";

const [wasmPath, ...rawArgs] = process.argv.slice(2);
if (!wasmPath) {
    console.error("usage: node scripts/run_wasi.mjs <file.wasm> [args...]");
    process.exit(2);
}

const args = process.platform === "win32" ? rawArgs.map((a) => a.replaceAll("\\", "/")) : rawArgs;
const wasi = new WASI({
    version: "preview1",
    args: [wasmPath, ...args],
    env: {},
    preopens: { ".": process.cwd() },
});
const module = await WebAssembly.compile(await readFile(wasmPath));
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
process.exitCode = wasi.start(instance);
