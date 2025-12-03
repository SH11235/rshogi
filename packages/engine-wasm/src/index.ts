import type {
    EngineClient,
    EngineEvent,
    EngineEventHandler,
    EngineInitOptions,
    EngineStopMode,
    SearchHandle,
    SearchParams,
} from "@shogi/engine-client";
import { createMockEngineClient } from "@shogi/engine-client";

type WasmModuleSource = WebAssembly.Module | ArrayBuffer | Uint8Array | string;

export interface WasmEngineInitOptions extends EngineInitOptions {
    /**
     * Optional preloaded wasm module or URL. When omitted, the worker is expected to fetch it.
     */
    wasmModule?: WasmModuleSource;
    /**
     * Optional transposition table size (in MB).
     */
    ttSizeMb?: number;
    /**
     * Optional default MultiPV value applied on init.
     */
    multiPv?: number;
}

export interface WasmEngineClientOptions {
    stopMode?: EngineStopMode;
    workerFactory?: () => Worker;
    useMock?: boolean;
}

type BackendKind = "worker" | "mock";

type WorkerCommand =
    | { type: "init"; opts?: WasmEngineInitOptions; wasmModule?: WasmModuleSource }
    | { type: "loadPosition"; sfen: string; moves?: string[] }
    | { type: "search"; params: SearchParams }
    | { type: "stop" }
    | { type: "dispose" }
    | { type: "setOption"; name: string; value: string | number | boolean };

function defaultWorkerFactory(): Worker {
    // Use the emitted JS file; pointing at .ts breaks when consuming the built package.
    return new Worker(new URL("./engine.worker.js", import.meta.url), { type: "module" });
}

function collectTransfers(command: WorkerCommand): Transferable[] {
    if (command.type === "init" && command.wasmModule) {
        if (command.wasmModule instanceof ArrayBuffer) {
            return [command.wasmModule];
        }
        if (command.wasmModule instanceof Uint8Array) {
            return [command.wasmModule.buffer];
        }
    }
    return [];
}

function rememberInitOpts(opts?: WasmEngineInitOptions): WasmEngineInitOptions | undefined {
    if (!opts) return undefined;
    const { wasmModule, ...rest } = opts;
    let preservedModule = wasmModule;
    if (wasmModule instanceof ArrayBuffer || wasmModule instanceof Uint8Array) {
        preservedModule = undefined;
    }
    return { ...rest, wasmModule: preservedModule };
}

export function createWasmEngineClient(options: WasmEngineClientOptions = {}): EngineClient {
    const stopMode: EngineStopMode = options.stopMode ?? "terminate";
    const mock = createMockEngineClient();
    const listeners = new Set<EngineEventHandler>();

    let backend: BackendKind = options.useMock ? "mock" : "worker";
    let worker: Worker | null = null;
    let initialized = false;
    let lastInitOpts: WasmEngineInitOptions | undefined;

    const emit = (event: EngineEvent) => {
        for (const handler of listeners) {
            handler(event);
        }
    };

    let mockUnsubscribe: (() => void) | null = null;

    const attachMock = () => {
        if (mockUnsubscribe) return;
        mockUnsubscribe = mock.subscribe(emit);
    };

    const detachMock = () => {
        if (!mockUnsubscribe) return;
        mockUnsubscribe();
        mockUnsubscribe = null;
    };

    if (backend === "mock") {
        attachMock();
    }

    const fallbackToMock = () => {
        terminateWorker();
        backend = "mock";
        attachMock();
    };

    const ensureWorker = () => {
        if (backend === "mock" || worker) return;
        try {
            worker = (options.workerFactory ?? defaultWorkerFactory)();
            worker.onmessage = (msg) => {
                const data = msg.data as { type: "event"; payload: EngineEvent };
                if (data?.type === "event" && data.payload) {
                    emit(data.payload);
                }
            };
            worker.onerror = (err) => {
                // Log worker errors in non-browser tools without referencing process types in the bundle.
                if (typeof console !== "undefined") console.error("engine worker error", err);
                emit({ type: "error", message: "Engine worker encountered an error" });
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
        const transfer = collectTransfers(command);
        if (transfer.length > 0) {
            worker.postMessage(command, transfer);
        } else {
            worker.postMessage(command);
        }
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

    const ensureReady = () => {
        if (backend === "worker" && !worker) {
            ensureWorker();
        }
        if (backend === "worker" && worker && !initialized) {
            const payload: WasmEngineInitOptions = lastInitOpts ?? { backend: "wasm" };
            postToWorker({ type: "init", opts: payload, wasmModule: payload.wasmModule });
            initialized = true;
        }
        // TODO: consider explicit worker state machine (uninitialized/ready/error) to simplify transitions.
    };

    return {
        async init(opts?: WasmEngineInitOptions) {
            const wasmOpts = opts as WasmEngineInitOptions | undefined;
            lastInitOpts = rememberInitOpts(wasmOpts);
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
            const payload: WasmEngineInitOptions = wasmOpts ?? { backend: "wasm" };
            postToWorker({ type: "init", opts: payload, wasmModule: wasmOpts?.wasmModule });
            initialized = true;
        },
        async loadPosition(sfen: string, moves?: string[]) {
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
                        // TODO: 探索中の wasm を協調停止できるようにする（SAB/Atomics などで停止フラグを即時伝搬）。
                        // 現状 runSearch がブロッキングで stop メッセージを処理できないため terminate にフォールバックする。
                        emit({
                            type: "error",
                            message:
                                "cooperative stop is not yet supported for wasm; falling back to terminate",
                        });
                        terminateWorker();
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
                // TODO: 協調停止対応を追加する。現状 stop メッセージが探索中に処理されないので terminate にフォールバック。
                emit({
                    type: "error",
                    message:
                        "cooperative stop is not yet supported for wasm; falling back to terminate",
                });
                terminateWorker();
            }
        },
        async setOption(name: string, value: string | number | boolean) {
            if (backend === "mock") {
                return mock.setOption(name, value);
            }
            ensureReady();
            if (!worker) {
                return mock.setOption(name, value);
            }
            postToWorker({ type: "setOption", name, value });
        },
        subscribe(handler: EngineEventHandler) {
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
