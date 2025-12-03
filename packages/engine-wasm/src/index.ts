import {
    EngineClient,
    EngineEvent,
    EngineEventHandler,
    EngineInitOptions,
    EngineStopMode,
    SearchHandle,
    SearchParams,
    createMockEngineClient,
} from "@shogi/engine-client";

export interface WasmEngineInitOptions extends EngineInitOptions {
    /**
     * Optional preloaded wasm module or URL. When omitted, the worker is expected to fetch it.
     */
    wasmModule?: WebAssembly.Module | ArrayBuffer | Uint8Array | string;
    /**
     * Optional factory for creating the Worker that hosts the wasm engine.
     */
    workerFactory?: () => Worker;
}

export interface WasmEngineClientOptions {
    stopMode?: EngineStopMode;
    workerFactory?: () => Worker;
    useMock?: boolean;
}

type BackendKind = "worker" | "mock";

type WorkerCommand =
    | { type: "init"; opts?: EngineInitOptions }
    | { type: "loadPosition"; sfen: string; moves?: string[] }
    | { type: "search"; params: SearchParams }
    | { type: "stop" }
    | { type: "dispose" };

/**
    The worker implementation currently uses the mock engine internally.
    Later this worker will wrap wasm-bindgen output and hide the stopMode (terminate/cooperative) strategy.
*/
function defaultWorkerFactory(): Worker {
    return new Worker(new URL("./engine.worker.ts", import.meta.url), { type: "module" });
}

export function createWasmEngineClient(options: WasmEngineClientOptions = {}): EngineClient {
    const stopMode: EngineStopMode = options.stopMode ?? "terminate";
    const useMock = options.useMock ?? false;
    const mock = createMockEngineClient();
    const listeners = new Set<EngineEventHandler>();

    let backend: BackendKind = useMock ? "mock" : "worker";
    let worker: Worker | null = null;
    let initialized = false;
    let lastInitOpts: EngineInitOptions | undefined;

    const emit = (event: EngineEvent) => {
        listeners.forEach((handler) => handler(event));
    };

    let mockUnsubscribe: (() => void) | null = null;

    const attachMock = () => {
        if (mockUnsubscribe) return;
        mockUnsubscribe = mock.subscribe(emit);
    };

    const detachMock = () => {
        if (mockUnsubscribe) {
            mockUnsubscribe();
            mockUnsubscribe = null;
        }
    };

    if (backend === "mock") {
        attachMock();
    }

    const ensureWorker = () => {
        if (backend === "mock") return;
        if (worker) return;
        try {
            worker = (options.workerFactory ?? defaultWorkerFactory)();
            worker.onmessage = (msg) => {
                const data = msg.data as { type: "event"; payload: EngineEvent };
                if (data?.type === "event" && data.payload) {
                    emit(data.payload);
                }
            };
            worker.onerror = (err) => {
                console.error("engine worker error", err);
                fallbackToMock();
            };
        } catch (error) {
            console.error("engine worker spawn failed, falling back to mock", error);
            fallbackToMock();
        }
    };

    const postToWorker = (command: WorkerCommand) => {
        if (!worker) {
            throw new Error("Wasm engine worker is not initialized");
        }
        worker.postMessage(command);
    };

    const terminateWorker = () => {
        if (worker) {
            try {
                worker.terminate();
            } catch {
                // ignore terminate errors
            }
            worker = null;
        }
        initialized = false;
    };

    const fallbackToMock = () => {
        terminateWorker();
        backend = "mock";
        attachMock();
    };

    const ensureReady = () => {
        if (backend === "worker" && !worker) {
            ensureWorker();
        }
        if (backend === "worker" && !initialized && lastInitOpts) {
            postToWorker({ type: "init", opts: lastInitOpts });
            initialized = true;
        }
    };

    return {
        async init(opts) {
            lastInitOpts = opts;
            if (backend === "mock") {
                await mock.init(opts);
                return;
            }
            ensureWorker();
            if (!worker) {
                await mock.init(opts);
                backend = "mock";
                return;
            }
            postToWorker({ type: "init", opts });
            initialized = true;
        },
        async loadPosition(sfen, moves) {
            if (backend === "mock") {
                return mock.loadPosition(sfen, moves);
            }
            ensureReady();
            if (!worker) {
                return mock.loadPosition(sfen, moves);
            }
            postToWorker({ type: "loadPosition", sfen, moves });
        },
        async search(params: SearchParams): Promise<SearchHandle> {
            if (backend === "mock") {
                return mock.search(params);
            }
            ensureReady();
            if (!worker) {
                return mock.search(params);
            }
            postToWorker({ type: "search", params });
            return {
                async cancel() {
                    if (backend === "mock") {
                        return mock.stop();
                    }
                    if (stopMode === "terminate") {
                        terminateWorker();
                    } else {
                        try {
                            postToWorker({ type: "stop" });
                        } catch {
                            terminateWorker();
                        }
                    }
                },
            };
        },
        async stop() {
            if (backend === "mock") {
                return mock.stop();
            }
            if (!worker) return;
            if (stopMode === "terminate") {
                terminateWorker();
            } else {
                try {
                    postToWorker({ type: "stop" });
                } catch {
                    terminateWorker();
                }
            }
        },
        async setOption(name, value) {
            // TODO: forward options to wasm once implemented
            if (backend === "mock") {
                return mock.setOption(name, value);
            }
            ensureReady();
            // Currently no-op for worker mock
            return;
        },
        subscribe(handler) {
            listeners.add(handler);
            return () => listeners.delete(handler);
        },
        async dispose() {
            if (backend === "mock") {
                await mock.dispose();
                detachMock();
                return;
            }
            if (worker) {
                try {
                    postToWorker({ type: "dispose" });
                } catch {
                    // ignore
                }
                terminateWorker();
            }
            detachMock();
            listeners.clear();
        },
    };
}
