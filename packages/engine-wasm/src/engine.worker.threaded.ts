import initWasm, {
    apply_moves as applyMoves,
    dispose as disposeEngine,
    init as initEngine,
    load_model as loadModel,
    load_position as loadPosition,
    search as runSearch,
    set_event_handler as setEventHandler,
    set_option as setOption,
    stop as stopEngine,
} from "../pkg-threaded/engine_wasm.js";
import { createEngineWorker } from "./engine.worker.base";

type WasmExports = { memory?: WebAssembly.Memory };
type InitInput = Parameters<typeof initWasm>[0];

const initWasmWithCache = async (input?: InitInput) => {
    const exports = (await initWasm(input)) as WasmExports;
    return exports;
};

// wasm_thread crate handles worker spawning automatically when Rust code calls
// wasm_thread::Builder::new().spawn(). No manual worker pool setup is needed.
// The poolSize parameter is kept for API compatibility but is not used here.
const initThreadPool = async (_poolSize: number) => {
    // No-op: wasm_thread manages thread spawning internally
};

createEngineWorker({
    initWasm: initWasmWithCache,
    applyMoves,
    disposeEngine,
    initEngine,
    loadModel,
    loadPosition,
    runSearch,
    setEventHandler,
    setOption,
    stopEngine,
    initThreadPool,
    defaultWasmModuleUrl: new URL("../pkg-threaded/engine_wasm_bg.wasm", import.meta.url),
});
