// WASI 向けにビルドした wasm を Node.js で動かす (ブラウザと同じ V8 で wasm の速さを測る)。
// usage: node scripts/run_wasi.mjs <file.wasm> [args...]
// 引数に出てくる既存のファイル・ディレクトリの親ディレクトリと、カレントディレクトリを wasm から見えるようにする。
import { readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";
import { WASI } from "node:wasi";

const [wasmPath, ...args] = process.argv.slice(2);
if (!wasmPath) {
    console.error("usage: node scripts/run_wasi.mjs <file.wasm> [args...]");
    process.exit(2);
}

const preopens = { ".": process.cwd() };
for (const arg of args) {
    const dir = path.dirname(arg);
    if (dir !== "." && existsSync(dir)) {
        preopens[dir] = path.resolve(dir);
    }
}

const wasi = new WASI({ version: "preview1", args: [wasmPath, ...args], env: process.env, preopens });
const module = await WebAssembly.compile(await readFile(wasmPath));
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
process.exitCode = wasi.start(instance);
