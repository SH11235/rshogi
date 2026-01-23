import type {
    EngineBackendStatus,
    EngineClient,
    EngineErrorCode,
    EngineEvent,
    EngineEventHandler,
    EngineInitOptions,
    EngineStopMode,
    LoadPositionOptions,
    SearchHandle,
    SearchParams,
    ThreadInfo,
} from "@shogi/engine-client";
import { createMockEngineClient } from "@shogi/engine-client";
import initWasmModule from "../pkg/engine_wasm.js";

type WasmModuleSource = WebAssembly.Module | ArrayBuffer | Uint8Array | string | URL;

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
    /**
     * Optional Worker factory. Receives the desired worker kind.
     */
    workerFactory?: (kind: WorkerKind) => Worker;
    useMock?: boolean;
    /**
     * Emit warning events to console for developers.
     */
    logWarningsToConsole?: boolean;
    /**
     * Default init() options applied when callers omit init params.
     */
    defaultInitOptions?: WasmEngineInitOptions;
}

type WorkerKind = "single" | "threaded";

type BackendKind = WorkerKind | "mock" | "error";

/**
 * エラーメッセージからエラーコードを推測する
 */
function classifyErrorCode(error: unknown): EngineErrorCode {
    const message =
        error instanceof Error ? error.message.toLowerCase() : String(error).toLowerCase();

    // ネットワーク関連エラー
    if (
        message.includes("fetch") ||
        message.includes("network") ||
        message.includes("failed to load") ||
        message.includes("networkerror") ||
        message.includes("cors") ||
        message.includes("not found") ||
        message.includes("404")
    ) {
        return "WASM_NETWORK_ERROR";
    }

    // メモリ関連エラー
    if (
        message.includes("memory") ||
        message.includes("out of memory") ||
        message.includes("oom") ||
        message.includes("allocation") ||
        message.includes("sharedarraybuffer")
    ) {
        return "WASM_MEMORY_ERROR";
    }

    // Worker生成エラー
    if (
        message.includes("worker") ||
        message.includes("spawn") ||
        message.includes("create worker")
    ) {
        return "WASM_WORKER_SPAWN_ERROR";
    }

    // タイムアウト
    if (message.includes("timeout") || message.includes("timed out")) {
        return "WASM_INIT_TIMEOUT";
    }

    // デフォルト
    return "WASM_INIT_FAILED";
}

/**
 * NNUE ロード元の種別
 */
export type NnueLoadSource =
    | { type: "idb"; id: string } // IndexedDB から
    | { type: "url"; url: string } // URL から fetch
    | { type: "bytes"; bytes: Uint8Array }; // 直接バイト列（transferable）

type WorkerCommand =
    | {
          type: "init";
          opts?: WasmEngineInitOptions;
          wasmModule?: WasmModuleSource;
          requestId?: string;
      }
    | {
          type: "loadPosition";
          sfen: string;
          moves?: string[];
          passRights?: { sente: number; gote: number };
          requestId?: string;
      }
    | { type: "applyMoves"; moves: string[]; requestId?: string }
    | { type: "search"; params: SearchParams; requestId?: string }
    | { type: "stop"; requestId?: string }
    | { type: "dispose"; requestId?: string }
    | { type: "setOption"; name: string; value: string | number | boolean; requestId?: string }
    | { type: "loadNnue"; source: NnueLoadSource; requestId?: string };

type WorkerCommandPayload =
    | { type: "init"; opts?: WasmEngineInitOptions; wasmModule?: WasmModuleSource }
    | {
          type: "loadPosition";
          sfen: string;
          moves?: string[];
          passRights?: { sente: number; gote: number };
      }
    | { type: "applyMoves"; moves: string[] }
    | { type: "search"; params: SearchParams }
    | { type: "stop" }
    | { type: "dispose" }
    | { type: "setOption"; name: string; value: string | number | boolean }
    | { type: "loadNnue"; source: NnueLoadSource };

type WorkerAck = { type: "ack"; requestId: string; error?: string };

type WorkerMessage =
    | { type: "event"; payload: EngineEvent }
    | { type: "events"; payload: EngineEvent[] }
    | WorkerAck
    | { type: "nnueLoadProgress"; loaded: number; total: number }
    | { type: "nnueLoaded"; size: number };

function defaultWorkerFactory(kind: WorkerKind): Worker {
    // IMPORTANT: Vite requires `new URL(..., import.meta.url)` to be used directly
    // inside `new Worker()` call for proper bundling. Pre-generating the URL
    // as a variable prevents Vite from recognizing it as a Worker entry point.
    //
    // Worker selection is based on crossOriginIsolated availability:
    // - GH Pages (no COOP/COEP headers): crossOriginIsolated=false → kind="single"
    // - Local dev with headers: crossOriginIsolated=true → kind="threaded" (if threads>1)
    if (kind === "threaded") {
        return new Worker(new URL("./engine.worker.threaded.js", import.meta.url), {
            type: "module",
        });
    }
    return new Worker(new URL("./engine.worker.single.js", import.meta.url), { type: "module" });
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
    if (command.type === "loadNnue" && command.source.type === "bytes") {
        return [command.source.bytes.buffer];
    }
    return [];
}

function rememberInitOpts(opts?: WasmEngineInitOptions): WasmEngineInitOptions | undefined {
    if (!opts) return undefined;
    const { wasmModule, ...rest } = opts;
    if (!Object.hasOwn(opts, "wasmModule")) {
        return { ...rest };
    }
    if (wasmModule instanceof ArrayBuffer || wasmModule instanceof Uint8Array) {
        return { ...rest, wasmModule: undefined };
    }
    return { ...rest, wasmModule };
}

let wasmModuleReady: Promise<void> | null = null;

export const ensureWasmModule = (wasmModule?: WasmModuleSource): Promise<void> => {
    if (!wasmModuleReady) {
        const moduleOrPath = wasmModule ?? new URL("../pkg/engine_wasm_bg.wasm", import.meta.url);
        wasmModuleReady = initWasmModule({ module_or_path: moduleOrPath }).then(() => undefined);
    }
    return wasmModuleReady;
};

const MSG_COOPERATIVE_STOP_NOT_SUPPORTED =
    "cooperative stop is not yet supported for wasm; falling back to terminate";
const DEFAULT_WORKER_TIMEOUT_MS = 30_000; // Worker リクエストのタイムアウト（30秒）

// Maximum threads for wasm: limited by browser implementation and memory constraints.
// - Chrome/Edge: 4-8 threads are typically stable
// - Firefox: similar limitations apply
// - Higher values may cause memory allocation failures or performance degradation
// - Conservative limit of 4 balances performance gains with stability
const MAX_WASM_THREADS = 4;

/**
 * NNUE ロード進捗イベント
 */
export interface NnueLoadProgressEvent {
    type: "progress";
    loaded: number;
    total: number;
}

/**
 * NNUE ロード完了イベント
 */
export interface NnueLoadedEvent {
    type: "loaded";
    size: number;
}

/**
 * NNUE ロードイベント
 */
export type NnueLoadEvent = NnueLoadProgressEvent | NnueLoadedEvent;

/**
 * NNUE ロードイベントハンドラー
 */
export type NnueLoadEventHandler = (event: NnueLoadEvent) => void;

/**
 * Wasm エンジンクライアントの拡張インターフェース
 * NNUE ロード進捗の購読機能を追加
 */
export interface WasmEngineClient extends EngineClient {
    /**
     * NNUE ロードイベントを購読
     * @param handler イベントハンドラー
     * @returns 購読解除関数
     */
    subscribeNnueLoad(handler: NnueLoadEventHandler): () => void;
}

export function createWasmEngineClient(options: WasmEngineClientOptions = {}): WasmEngineClient {
    // NOTE: stopMode のデフォルトは "terminate"。"cooperative" は未実装のため内部で terminate にフォールバックする。
    const stopMode: EngineStopMode = options.stopMode ?? "terminate";
    const mock = createMockEngineClient();
    const listeners = new Set<EngineEventHandler>();
    const nnueLoadListeners = new Set<NnueLoadEventHandler>();
    const logWarningsToConsole = options.logWarningsToConsole ?? false;

    let backend: BackendKind = options.useMock ? "mock" : "single";
    let worker: Worker | null = null;
    let workerGen = 0;
    let initialized = false;
    let initInFlight: Promise<void> | null = null;
    let lastInitOpts: WasmEngineInitOptions | undefined = rememberInitOpts(
        options.defaultInitOptions,
    );
    let lastPosition: {
        sfen: string;
        moves: string[];
        passRights?: { sente: number; gote: number };
    } | null = null;
    let threadedDisabled = false;
    let activeThreads: number | null = null;

    // Cache for getThreadInfo() - hardwareConcurrency and threadedAvailable rarely change
    let cachedStaticThreadInfo: {
        hardwareConcurrency: number;
        maxThreads: number;
        threadedAvailable: boolean;
    } | null = null;

    const pendingOptions = new Map<string, string | number | boolean>();
    const warnedReasons = new Set<string>();

    const emitNnueLoad = (event: NnueLoadEvent) => {
        for (const handler of nnueLoadListeners) {
            handler(event);
        }
    };

    const emit = (event: EngineEvent) => {
        if (event.type === "error" && event.severity !== "warning") {
            lastPosition = null;
        }
        for (const handler of listeners) {
            handler(event);
        }
    };

    const emitWarn = (code: EngineErrorCode, message: string) => {
        if (warnedReasons.has(code)) return;
        warnedReasons.add(code);
        if (logWarningsToConsole && typeof console !== "undefined") {
            console.warn(`[engine-wasm] ${code}: ${message}`);
        }
        emit({ type: "error", message, severity: "warning", code });
    };

    const emitError = (code: EngineErrorCode, message: string) => {
        emit({ type: "error", message, severity: "error", code });
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

    const normalizeOptionName = (name: string) => {
        if (name.toLowerCase() === "threads") return "Threads";
        return name;
    };

    const mergeInitOptions = (opts?: WasmEngineInitOptions) => {
        if (!opts) return;
        const preserved = rememberInitOpts(opts);
        if (!preserved) return;
        const next: WasmEngineInitOptions = { ...lastInitOpts, ...preserved };
        if (!Object.hasOwn(opts, "wasmModule") && lastInitOpts?.wasmModule) {
            next.wasmModule = lastInitOpts.wasmModule;
        }
        lastInitOpts = next;
    };

    const parseThreadsValue = (value: unknown): number | undefined => {
        if (typeof value === "number" && Number.isFinite(value)) {
            return Math.trunc(value);
        }
        if (typeof value === "string" && value.trim() !== "") {
            const parsed = Number(value);
            if (Number.isFinite(parsed)) {
                return Math.trunc(parsed);
            }
        }
        return undefined;
    };

    const getIncrementalMoves = (prev: string[], next: string[]): string[] | null => {
        if (prev.length > next.length) return null;
        for (let i = 0; i < prev.length; i += 1) {
            if (prev[i] !== next[i]) return null;
        }
        return next.slice(prev.length);
    };

    const getThreadedAvailability = () => {
        if (threadedDisabled) return false;
        if (typeof crossOriginIsolated === "undefined" || !crossOriginIsolated) return false;
        if (typeof SharedArrayBuffer === "undefined") return false;
        return true;
    };

    const computeEffectiveThreads = (requested?: number) => {
        const desired = parseThreadsValue(requested) ?? 1;
        const threadedAvailable = getThreadedAvailability();
        if (!threadedAvailable) {
            if (desired > 1 && !threadedDisabled) {
                emitWarn(
                    "WASM_THREADS_UNAVAILABLE",
                    "Wasm threads unavailable (crossOriginIsolated=false or SharedArrayBuffer unsupported); falling back to single-threaded engine.",
                );
            }
            return { effective: 1, threadedAvailable: false };
        }
        const hcRaw =
            typeof navigator !== "undefined" && typeof navigator.hardwareConcurrency === "number"
                ? navigator.hardwareConcurrency
                : 1;
        const hc = Math.max(1, Math.trunc(hcRaw));
        const max = Math.max(1, Math.min(MAX_WASM_THREADS, hc));
        let effective = desired;
        if (effective < 1) effective = 1;
        if (effective > max) effective = max;
        if (desired !== effective && requested !== undefined) {
            emitWarn(
                "WASM_THREADS_CLAMPED",
                `Threads requested=${desired} exceeds max=${max}; using ${effective}.`,
            );
        }
        return { effective, threadedAvailable: true };
    };

    const shouldUseThreadedWorker = (effectiveThreads: number) => effectiveThreads > 1;

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

    const postToWorkerAwait = async (
        command: WorkerCommandPayload,
        timeoutMs = DEFAULT_WORKER_TIMEOUT_MS,
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

    const replaceWorker = (reason: string) => {
        if (worker) {
            try {
                worker.terminate();
            } catch {
                // ignore terminate errors
            }
            worker = null;
        }
        workerGen += 1;
        initialized = false;
        activeThreads = null;
        rejectAllPending(new Error(reason));
    };

    const enterErrorState = (message: string, code: EngineErrorCode) => {
        replaceWorker("engine worker is unavailable");
        initInFlight = null;
        backend = "error";
        emitError(code, message);
        // Do not attach mock - engine is in error state
    };

    const spawnWorker = (kind: WorkerKind, forceReplace = false) => {
        if (backend === "mock" || backend === "error") return;
        if (!forceReplace && worker && backend === kind) return;

        if (forceReplace || worker || backend !== kind) {
            replaceWorker("worker replaced");
        }
        backend = kind;

        const gen = workerGen;
        try {
            worker = options.workerFactory
                ? options.workerFactory(kind)
                : defaultWorkerFactory(kind);
        } catch (error) {
            const message = error instanceof Error ? error.message : "engine worker spawn failed";
            const code = classifyErrorCode(error);
            enterErrorState(message, code);
            return;
        }

        worker.onmessage = (msg: MessageEvent) => {
            if (gen !== workerGen) return;
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
                return;
            }
            if (data?.type === "nnueLoadProgress") {
                emitNnueLoad({
                    type: "progress",
                    loaded: data.loaded,
                    total: data.total,
                });
                return;
            }
            if (data?.type === "nnueLoaded") {
                emitNnueLoad({
                    type: "loaded",
                    size: data.size,
                });
                return;
            }
        };

        worker.onerror = (err) => {
            if (gen !== workerGen) return;
            emitWarn("WASM_WORKER_FAILED", "Engine worker encountered an error.");
            if (backend === "threaded") {
                threadedDisabled = true;
                void recoverWorker("single");
                return;
            }
            const code = classifyErrorCode(err);
            enterErrorState("Engine worker encountered an error", code);
            if (typeof console !== "undefined") console.error("engine worker error", err);
        };
    };

    const buildInitPayload = (effectiveThreads: number): WasmEngineInitOptions => {
        const { wasmModule: _ignored, ...rest } = lastInitOpts ?? {};
        return { ...rest, backend: "wasm", threads: effectiveThreads };
    };

    const getInitWasmModule = (opts?: WasmEngineInitOptions): WasmModuleSource | undefined => {
        if (opts && Object.hasOwn(opts, "wasmModule")) {
            return opts.wasmModule;
        }
        return lastInitOpts?.wasmModule;
    };

    const applyPendingOptions = async () => {
        if (!worker) return;
        for (const [name, value] of pendingOptions.entries()) {
            await postToWorkerAwait({ type: "setOption", name, value });
        }
    };

    const restoreLastPosition = async () => {
        if (!worker || !lastPosition) return;
        await postToWorkerAwait({
            type: "loadPosition",
            sfen: lastPosition.sfen,
            moves: lastPosition.moves,
            passRights: lastPosition.passRights,
        });
    };

    const initWorkerWithKind = async (
        kind: WorkerKind,
        opts: WasmEngineInitOptions,
        wasmModule?: WasmModuleSource,
        forceReplace = false,
    ) => {
        spawnWorker(kind, forceReplace);
        if (!worker || backend === "mock") {
            throw new Error("engine worker is unavailable");
        }
        await postToWorkerAwait({ type: "init", opts, wasmModule });
        initialized = true;
        await applyPendingOptions();
        await restoreLastPosition();
        activeThreads = opts.threads ?? 1;
    };

    const recoverWorker = async (kind: WorkerKind) => {
        if (backend === "mock") return;
        const requestedThreads = lastInitOpts?.threads;
        const { effective } = computeEffectiveThreads(requestedThreads);
        const effectiveThreads = kind === "threaded" ? effective : 1;
        const payload = buildInitPayload(effectiveThreads);
        const module = getInitWasmModule();
        try {
            await initWorkerWithKind(kind, payload, module, true);
        } catch (error) {
            const code = classifyErrorCode(error);
            enterErrorState("Wasm engine initialization failed", code);
        }
    };

    const startInit = async (opts?: WasmEngineInitOptions) => {
        mergeInitOptions(opts);
        const requestedThreads = lastInitOpts?.threads;
        const { effective } = computeEffectiveThreads(requestedThreads);
        const payload = buildInitPayload(effective);
        const module = getInitWasmModule(opts);
        const desiredKind = shouldUseThreadedWorker(effective) ? "threaded" : "single";
        const forceReplace =
            backend !== "mock" &&
            worker != null &&
            backend === desiredKind &&
            activeThreads != null &&
            activeThreads !== effective;

        if (backend === "mock") {
            await mock.init(opts);
            return;
        }

        initialized = false;

        try {
            await initWorkerWithKind(desiredKind, payload, module, forceReplace);
            return;
        } catch (error) {
            if ((backend as BackendKind) === "mock") {
                await mock.init(opts);
                return;
            }
            if (desiredKind === "threaded") {
                emitWarn(
                    "WASM_THREADS_INIT_FAILED",
                    "Threaded wasm initialization failed; falling back to single-threaded engine.",
                );
                threadedDisabled = true;
                const fallbackPayload = buildInitPayload(1);
                const fallbackModule = getInitWasmModule();
                try {
                    await initWorkerWithKind("single", fallbackPayload, fallbackModule);
                } catch (fallbackError) {
                    const fallbackCode = classifyErrorCode(fallbackError);
                    enterErrorState("Wasm engine initialization failed", fallbackCode);
                }
                return;
            }
            throw error;
        }
    };

    const ensureReady = async () => {
        if (backend === "mock") return;
        if (initialized) return;
        if (initInFlight) {
            await initInFlight;
            return;
        }
        initInFlight = startInit();
        try {
            await initInFlight;
        } finally {
            initInFlight = null;
        }
    };

    const terminateAndRecover = () => {
        if (backend === "mock") return;
        replaceWorker("engine worker terminated");
        initInFlight = null;
        void ensureReady().catch((error) => {
            const code = classifyErrorCode(error);
            enterErrorState("Wasm engine initialization failed", code);
        });
    };

    return {
        async init(opts?: WasmEngineInitOptions) {
            if (backend === "mock") {
                await mock.init(opts);
                return;
            }
            // Allow retry from error state
            if (backend === "error") {
                // Preserve previous worker kind preference if available
                const preferredKind: WorkerKind = threadedDisabled
                    ? "single"
                    : cachedStaticThreadInfo?.threadedAvailable
                      ? "threaded"
                      : "single";
                backend = preferredKind;
                initialized = false;
                warnedReasons.clear();
                // Keep cachedStaticThreadInfo as it's still valid
            }
            lastPosition = null;
            if (initInFlight) {
                await initInFlight;
            }
            initInFlight = startInit(opts);
            try {
                await initInFlight;
            } finally {
                initInFlight = null;
            }
        },
        async loadPosition(sfen: string, moves?: string[], options?: LoadPositionOptions) {
            if (backend === "mock") {
                return mock.loadPosition(sfen, moves, options);
            }
            if (backend === "error") {
                throw new Error(
                    "エンジンはエラー状態です。init()を呼び出してリトライしてください。",
                );
            }
            await ensureReady();
            if (!worker) {
                return mock.loadPosition(sfen, moves, options);
            }
            const normalizedMoves = moves ?? [];
            const passRights = options?.passRights;

            // パス権設定が変わった場合は差分適用ではなく完全再読み込み
            const passRightsChanged =
                lastPosition?.passRights?.sente !== passRights?.sente ||
                lastPosition?.passRights?.gote !== passRights?.gote;

            const incrementalMoves =
                lastPosition && lastPosition.sfen === sfen && !passRightsChanged
                    ? getIncrementalMoves(lastPosition.moves, normalizedMoves)
                    : null;
            if (incrementalMoves !== null) {
                if (incrementalMoves.length > 0) {
                    await postToWorkerAwait({ type: "applyMoves", moves: incrementalMoves });
                }
                lastPosition = { sfen, moves: normalizedMoves.slice(), passRights };
                return;
            }
            await postToWorkerAwait({
                type: "loadPosition",
                sfen,
                moves: normalizedMoves,
                passRights,
            });
            lastPosition = { sfen, moves: normalizedMoves.slice(), passRights };
        },
        async search(params: SearchParams): Promise<SearchHandle> {
            if (backend === "mock") {
                return mock.search(params);
            }
            if (backend === "error") {
                emit({
                    type: "error",
                    message: "エンジンはエラー状態です。init()を呼び出してリトライしてください。",
                    severity: "error",
                    code: "ENGINE_ERROR_STATE",
                });
                return {
                    async cancel() {
                        /* no-op */
                    },
                };
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
                        terminateAndRecover();
                    } else {
                        if (typeof console !== "undefined") {
                            console.warn(MSG_COOPERATIVE_STOP_NOT_SUPPORTED);
                        }
                        terminateAndRecover();
                    }
                },
            };
        },
        async stop() {
            if (backend === "mock") {
                return mock.stop();
            }
            if (backend === "error") {
                return; // no-op in error state
            }
            if (stopMode === "terminate") {
                terminateAndRecover();
            } else {
                if (typeof console !== "undefined") {
                    console.warn(MSG_COOPERATIVE_STOP_NOT_SUPPORTED);
                }
                terminateAndRecover();
            }
        },
        async setOption(name: string, value: string | number | boolean) {
            const normalized = normalizeOptionName(name);
            if (normalized === "Threads") {
                const parsed = parseThreadsValue(value);
                if (parsed !== undefined) {
                    lastInitOpts = { ...lastInitOpts, threads: parsed };
                    if (initialized) {
                        emitWarn(
                            "WASM_THREADS_DEFERRED",
                            "Threads option is applied on the next init.",
                        );
                    }
                }
                return;
            }

            pendingOptions.set(normalized, value);

            if (backend === "mock") {
                return mock.setOption(name, value);
            }

            const initPromise = initInFlight;
            const wasInitialized = initialized;
            await ensureReady();
            if (!worker) {
                return mock.setOption(name, value);
            }
            if (wasInitialized && !initPromise) {
                await postToWorkerAwait({ type: "setOption", name: normalized, value });
            }
        },
        subscribe(handler: EngineEventHandler) {
            listeners.add(handler);
            return () => listeners.delete(handler);
        },
        /**
         * NNUE ロードイベントを購読
         */
        subscribeNnueLoad(handler: NnueLoadEventHandler) {
            nnueLoadListeners.add(handler);
            return () => nnueLoadListeners.delete(handler);
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
            }
            replaceWorker("engine worker disposed");
            detachMock();
            listeners.clear();
            nnueLoadListeners.clear();
            lastPosition = null;
            lastInitOpts = undefined;
            pendingOptions.clear();
            warnedReasons.clear();
            threadedDisabled = false;
            initInFlight = null;
        },
        async reset(): Promise<void> {
            // Cancel any in-flight operations
            if (initInFlight) {
                initInFlight = null;
            }
            rejectAllPending(new Error("Engine reset requested"));

            // Terminate existing worker
            if (worker) {
                replaceWorker("reset requested");
            }

            // Clear error or mock state
            if (backend === "error" || backend === "mock") {
                backend = "single";
            }

            initialized = false;
            threadedDisabled = false;
            warnedReasons.clear();
            // Keep cachedStaticThreadInfo as hardware capabilities don't change
            // Note: Does not call init() - caller should explicitly call init() after reset()
        },
        getBackendStatus(): EngineBackendStatus {
            if (backend === "error") return "error";
            if (backend === "mock") return "mock";
            // Return "ready" for both initialized and uninitialized normal states
            return "ready";
        },
        getThreadInfo(): ThreadInfo {
            // Error state: return minimal thread info
            if (backend === "error") {
                const hcRaw =
                    typeof navigator !== "undefined" &&
                    typeof navigator.hardwareConcurrency === "number"
                        ? navigator.hardwareConcurrency
                        : 1;
                return {
                    activeThreads: 0,
                    maxThreads: 0,
                    threadedAvailable: false,
                    hardwareConcurrency: Math.max(1, Math.trunc(hcRaw)),
                };
            }
            // Use cached static values (hardwareConcurrency, threadedAvailable rarely change)
            if (!cachedStaticThreadInfo) {
                const hcRaw =
                    typeof navigator !== "undefined" &&
                    typeof navigator.hardwareConcurrency === "number"
                        ? navigator.hardwareConcurrency
                        : 1;
                const hardwareConcurrency = Math.max(1, Math.trunc(hcRaw));
                const threadedAvailable = getThreadedAvailability();
                const maxThreads = threadedAvailable
                    ? Math.max(1, Math.min(MAX_WASM_THREADS, hardwareConcurrency))
                    : 1;
                cachedStaticThreadInfo = { hardwareConcurrency, maxThreads, threadedAvailable };
            }
            return {
                activeThreads: activeThreads ?? 1,
                ...cachedStaticThreadInfo,
            };
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

// NNUE フォーマット検出 API
export { detect_nnue_format, is_nnue_compatible } from "../pkg/engine_wasm.js";

// NNUE ストレージ
export { createIndexedDBNnueStorage, requestPersistentStorage } from "./nnue";
