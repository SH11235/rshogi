import { readFile } from "node:fs/promises";
import assert from "node:assert";

import initWasm, {
    init as initEngine,
    load_position as loadPosition,
    search as runSearch,
    set_event_handler as setEventHandler,
    stop as stopEngine,
} from "../pkg/engine_wasm.js";

function delay(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
}

const wasmModule = new URL("../pkg/engine_wasm_bg.wasm", import.meta.url);
const wasmBytes = await readFile(wasmModule);
await initWasm({ module_or_path: wasmBytes });
const events = [];
setEventHandler((event) => {
    events.push(event);
});
await initEngine(JSON.stringify({ tt_size_mb: 16, multi_pv: 1 }));
await loadPosition("startpos", undefined);

runSearch(JSON.stringify({ limits: { maxDepth: 1 } }));

let bestmove = null;
const timeoutAt = Date.now() + 5000;
while (!bestmove && Date.now() < timeoutAt) {
    const next = events.shift();
    if (next?.type === "bestmove") {
        bestmove = next.move;
        break;
    }
    await delay(50);
}

assert.ok(bestmove, "expected bestmove event from wasm search");
assert.notStrictEqual(bestmove, "resign", "engine should not resign at depth 1");

await stopEngine();

// eslint-disable-next-line no-console
console.log(`startpos depth=1 bestmove: ${bestmove}`);
