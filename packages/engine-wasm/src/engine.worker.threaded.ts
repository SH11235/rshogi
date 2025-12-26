import initWasm, {
    apply_moves as applyMoves,
    dispose as disposeEngine,
    init as initEngine,
    initThreadPool as initThreadPoolWasm,
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

// wasm-bindgen-rayon's init_thread_pool returns a Promise that resolves
// when all worker threads are ready. This handles the async Worker creation
// that caused deadlocks with the previous wasm_thread approach.
const initThreadPool = async (poolSize: number) => {
    await initThreadPoolWasm(poolSize);
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
