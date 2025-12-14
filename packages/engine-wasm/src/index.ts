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
import initWasmModule from "../pkg/engine_wasm.js";

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
    | {
          type: "init";
          opts?: WasmEngineInitOptions;
          wasmModule?: WasmModuleSource;
          requestId?: string;
      }
    | { type: "loadPosition"; sfen: string; moves?: string[]; requestId?: string }
    | { type: "applyMoves"; moves: string[]; requestId?: string }
    | { type: "search"; params: SearchParams; requestId?: string }
    | { type: "stop"; requestId?: string }
    | { type: "dispose"; requestId?: string }
    | { type: "setOption"; name: string; value: string | number | boolean; requestId?: string };

type WorkerCommandPayload =
    | { type: "init"; opts?: WasmEngineInitOptions; wasmModule?: WasmModuleSource }
    | { type: "loadPosition"; sfen: string; moves?: string[] }
    | { type: "applyMoves"; moves: string[] }
    | { type: "search"; params: SearchParams }
    | { type: "stop" }
    | { type: "dispose" }
    | { type: "setOption"; name: string; value: string | number | boolean };

type WorkerAck = { type: "ack"; requestId: string; error?: string };

type WorkerMessage =
    | { type: "event"; payload: EngineEvent }
    | { type: "events"; payload: EngineEvent[] }
    | WorkerAck;

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

let wasmModuleReady: Promise<void> | null = null;

export const ensureWasmModule = (wasmModule?: WasmModuleSource): Promise<void> => {
    if (!wasmModuleReady) {
        const moduleOrPath = wasmModule ?? new URL("../pkg/engine_wasm_bg.wasm", import.meta.url);
        wasmModuleReady = initWasmModule({ module_or_path: moduleOrPath }).then(() => undefined);
    }
    return wasmModuleReady;
};

export function createWasmEngineClient(options: WasmEngineClientOptions = {}): EngineClient {
    const stopMode: EngineStopMode = options.stopMode ?? "terminate";
    const mock = createMockEngineClient();
    const listeners = new Set<EngineEventHandler>();

    let backend: BackendKind = options.useMock ? "mock" : "worker";
    let worker: Worker | null = null;
    let initialized = false;
    let initInFlight: Promise<void> | null = null;
    let lastInitOpts: WasmEngineInitOptions | undefined;
    let lastPosition: { sfen: string; moves: string[] } | null = null;

    const emit = (event: EngineEvent) => {
        if (event.type === "error") {
            lastPosition = null;
        }
        for (const handler of listeners) {
            handler(event);
        }
    };

    const pendingAcks = new Map<string, { resolve: () => void; reject: (error: Error) => void }>();

    const rejectAllPending = (error: unknown) => {
        const err = error instanceof Error ? error : new Error(String(error));
        for (const pending of pendingAcks.values()) {
            pending.reject(err);
        }
        pendingAcks.clear();
    };

    const createRequestId = (): string => {
        if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
            return (crypto as { randomUUID: () => string }).randomUUID();
        }
        return `${Date.now()}-${Math.random()}`;
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
        rejectAllPending(new Error("engine worker is unavailable"));
        terminateWorker();
        backend = "mock";
        attachMock();
    };

    const ensureWorker = () => {
        if (backend === "mock" || worker) return;
        try {
            worker = (options.workerFactory ?? defaultWorkerFactory)();
            worker.onmessage = (msg: MessageEvent) => {
                const data = msg.data as WorkerMessage;
                if (data?.type === "ack" && data.requestId) {
                    const pending = pendingAcks.get(data.requestId);
                    if (!pending) return;
                    pendingAcks.delete(data.requestId);
                    if (data.error) {
                        pending.reject(new Error(data.error));
                    } else {
                        pending.resolve();
                    }
                    return;
                }
                if (data?.type === "event" && data.payload) {
                    emit(data.payload);
                    return;
                }
                if (data?.type === "events" && Array.isArray(data.payload)) {
                    for (const event of data.payload) {
                        emit(event);
                    }
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
        rejectAllPending(new Error("engine worker terminated"));
        initialized = false;
        initInFlight = null;
        lastPosition = null;
    };

    const postToWorkerAwait = async (
        command: WorkerCommandPayload,
        timeoutMs = 30_000,
    ): Promise<void> => {
        if (!worker) {
            throw new Error("Wasm engine worker is not initialized");
        }
        const requestId = createRequestId();
        return new Promise<void>((resolve, reject) => {
            const timeoutId =
                timeoutMs > 0
                    ? setTimeout(() => {
                          const pending = pendingAcks.get(requestId);
                          if (!pending) return;
                          pendingAcks.delete(requestId);
                          pending.reject(new Error(`Worker request timed out: ${command.type}`));
                      }, timeoutMs)
                    : null;

            pendingAcks.set(requestId, {
                resolve: () => {
                    if (timeoutId) clearTimeout(timeoutId);
                    resolve();
                },
                reject: (error) => {
                    if (timeoutId) clearTimeout(timeoutId);
                    reject(error);
                },
            });

            try {
                postToWorker({ ...command, requestId } as WorkerCommand);
            } catch (error) {
                pendingAcks.delete(requestId);
                if (timeoutId) clearTimeout(timeoutId);
                reject(error instanceof Error ? error : new Error(String(error)));
            }
        });
    };

    const ensureReady = async () => {
        if (backend === "worker" && !worker) {
            ensureWorker();
        }
        if (backend !== "worker" || !worker) return;
        if (initialized) return;
        if (initInFlight) {
            await initInFlight;
            return;
        }

        const payload: WasmEngineInitOptions = lastInitOpts ?? { backend: "wasm" };
        initInFlight = postToWorkerAwait({
            type: "init",
            opts: payload,
            wasmModule: payload.wasmModule,
        })
            .then(() => {
                initialized = true;
            })
            .finally(() => {
                initInFlight = null;
            });
        await initInFlight;
        // TODO: consider explicit worker state machine (uninitialized/ready/error) to simplify transitions.
    };

    return {
        async init(opts?: WasmEngineInitOptions) {
            const wasmOpts = opts as WasmEngineInitOptions | undefined;
            lastInitOpts = rememberInitOpts(wasmOpts);
            lastPosition = null;
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
            initialized = false;
            initInFlight = postToWorkerAwait({
                type: "init",
                opts: payload,
                wasmModule: wasmOpts?.wasmModule,
            })
                .then(() => {
                    initialized = true;
                })
                .finally(() => {
                    initInFlight = null;
                });
            await initInFlight;
        },
        async loadPosition(sfen: string, moves?: string[]) {
            if (backend === "mock") {
                return mock.loadPosition(sfen, moves);
            }
            await ensureReady();
            if (!worker) {
                return mock.loadPosition(sfen, moves);
            }
            const normalizedMoves = moves ?? [];
            // TEMPORARY FIX: Disable incremental position loading to avoid state issues
            // Always load the full position instead of applying incremental moves
            void lastPosition; // Keep for reference but don't use
            // if (
            //     previous &&
            //     previous.sfen === sfen &&
            //     normalizedMoves.length >= previous.moves.length &&
            //     previous.moves.every((mv, idx) => normalizedMoves[idx] === mv)
            // ) {
            //     const delta = normalizedMoves.slice(previous.moves.length);
            //     if (delta.length) {
            //         await postToWorkerAwait({ type: "applyMoves", moves: delta });
            //         previous.moves.push(...delta);
            //     }
            //     return;
            // }

            await postToWorkerAwait({ type: "loadPosition", sfen, moves: normalizedMoves });
            lastPosition = { sfen, moves: normalizedMoves.slice() };
        },
        async search(params: SearchParams): Promise<SearchHandle> {
            if (backend === "mock") {
                return mock.search(params);
            }
            await ensureReady();
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
            lastPosition = null;
        },
        async setOption(name: string, value: string | number | boolean) {
            if (backend === "mock") {
                return mock.setOption(name, value);
            }
            await ensureReady();
            if (!worker) {
                return mock.setOption(name, value);
            }
            await postToWorkerAwait({ type: "setOption", name, value });
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
            lastPosition = null;
        },
    };
}

export {
    wasm_board_to_sfen,
    wasm_get_initial_board,
    wasm_get_legal_moves,
    wasm_parse_sfen_to_board,
    wasm_replay_moves_strict,
} from "../pkg/engine_wasm.js";
