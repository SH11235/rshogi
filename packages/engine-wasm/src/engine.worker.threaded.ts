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

const DEFAULT_THREAD_STACK_SIZE = 2 * 1024 * 1024;

type WasmExports = { memory?: WebAssembly.Memory };
type InitInput = Parameters<typeof initWasm>[0];

let cachedModule: WebAssembly.Module | null = null;
let cachedExports: WasmExports | null = null;
let threadPoolPromise: Promise<void> | null = null;
const threadWorkers: Worker[] = [];

const initWasmWithCache = async (input?: InitInput) => {
    const exports = (await initWasm(input)) as WasmExports;
    cachedExports = exports;
    const module = (initWasm as unknown as { __wbindgen_wasm_module?: WebAssembly.Module })
        .__wbindgen_wasm_module;
    if (module instanceof WebAssembly.Module) {
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
            cachedModule ??
            (initWasm as unknown as { __wbindgen_wasm_module?: WebAssembly.Module })
                .__wbindgen_wasm_module;
        const memory = cachedExports?.memory;

        if (!module) {
            throw new Error("Wasm module is not initialized");
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
                const handleError = () => {
                    worker.removeEventListener("message", handleMessage);
                    reject(new Error("Thread worker initialization failed"));
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
