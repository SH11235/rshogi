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

// Thread stack size: 2MB per worker thread
// - Shogi search is recursive and can consume significant stack for deep tactical sequences
// - 2MB provides headroom for typical search depths while limiting memory footprint
// - Total memory impact: (threads - 1) * 2MB for worker pool
const DEFAULT_THREAD_STACK_SIZE = 2 * 1024 * 1024;

type WasmExports = { memory?: WebAssembly.Memory };
type InitInput = Parameters<typeof initWasm>[0];

let cachedModule: WebAssembly.Module | null = null;
let cachedExports: WasmExports | null = null;
let threadPoolPromise: Promise<void> | null = null;
const threadWorkers: Worker[] = [];

// HACK: Accessing internal wasm-bindgen property `__wbindgen_wasm_module`.
// This is not part of the public API and may break in future wasm-bindgen versions.
// Required to pass the compiled module to worker threads for SharedArrayBuffer-based threading.
// If this breaks, check wasm-bindgen release notes for alternative approaches.
type WasmBindgenInternal = { __wbindgen_wasm_module?: WebAssembly.Module };

const initWasmWithCache = async (input?: InitInput) => {
    const exports = (await initWasm(input)) as WasmExports;
    cachedExports = exports;
    const module = (initWasm as unknown as WasmBindgenInternal).__wbindgen_wasm_module;
    if (module && module instanceof WebAssembly.Module) {
        cachedModule = module;
    }
    return exports;
};

const initThreadPool = async (poolSize: number) => {
    if (poolSize <= 0) return;
    if (threadPoolPromise) {
        await threadPoolPromise;
        return;
    }

    threadPoolPromise = (async () => {
        const module =
            cachedModule ?? (initWasm as unknown as WasmBindgenInternal).__wbindgen_wasm_module;
        const memory = cachedExports?.memory;

        if (!module || !(module instanceof WebAssembly.Module)) {
            throw new Error("Wasm module is not initialized or invalid");
        }
        if (!memory || !(memory instanceof WebAssembly.Memory)) {
            throw new Error("Wasm memory is not initialized");
        }

        const workerUrl = new URL("../pkg-threaded/engine_wasm_worker.js", import.meta.url);

        const readyPromises = Array.from({ length: poolSize }, () => {
            const worker = new Worker(workerUrl, { type: "module" });
            threadWorkers.push(worker);
            const ready = new Promise<void>((resolve, reject) => {
                const handleMessage = (event: MessageEvent) => {
                    if (event.data?.type !== "ready") return;
                    worker.removeEventListener("message", handleMessage);
                    resolve();
                };
                const handleError = (event: ErrorEvent) => {
                    worker.removeEventListener("message", handleMessage);
                    worker.terminate();
                    const errorDetail = event.error ? `: ${event.error.message}` : "";
                    const errorInfo = event.filename ? ` at ${event.filename}:${event.lineno}` : "";
                    reject(
                        new Error(`Thread worker initialization failed${errorDetail}${errorInfo}`),
                    );
                };
                worker.addEventListener("message", handleMessage);
                worker.addEventListener("error", handleError, { once: true });
            });
            worker.postMessage({
                module,
                memory,
                thread_stack_size: DEFAULT_THREAD_STACK_SIZE,
            });
            return ready;
        });

        await Promise.all(readyPromises);
    })();

    await threadPoolPromise;
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
