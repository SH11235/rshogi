import type { EngineEvent, EngineInitOptions, SearchParams } from "@shogi/engine-client";
import { NNUE_DB_NAME, NNUE_DB_VERSION, NNUE_PROGRESS_THROTTLE_MS } from "@shogi/app-core";

type WasmModuleSource = WebAssembly.Module | ArrayBuffer | Uint8Array | string | URL;
type WasmInitInput =
    | WasmModuleSource
    | {
          module_or_path: WasmModuleSource;
          memory?: WebAssembly.Memory;
          thread_stack_size?: number;
      };

type PassRightsInput = { sente: number; gote: number };

type WasmWorkerBindings = {
    initWasm: (input?: WasmInitInput) => Promise<unknown>;
    applyMoves: (moves: string[]) => Promise<void> | void;
    disposeEngine: () => void;
    initEngine: (opts?: EngineInitOptions) => Promise<void> | void;
    loadModel: (bytes: Uint8Array) => Promise<void> | void;
    loadPosition: (
        sfen: string,
        moves?: string[],
        passRights?: PassRightsInput,
    ) => Promise<void> | void;
    runSearch: (params?: SearchParams) => Promise<void> | void;
    setEventHandler: (handler: (event: EngineEvent) => void) => void;
    setOption: (name: string, value: string | number | boolean) => Promise<void> | void;
    stopEngine: () => void;
    initThreadPool?: (poolSize: number) => Promise<void> | void;
    defaultWasmModuleUrl?: URL;
};

type CommandBase = { requestId?: string };

type InitCommand = CommandBase & {
    type: "init";
    opts?: EngineInitOptions;
    wasmModule?: WasmModuleSource;
};

/**
 * NNUE ロード元の種別
 */
type NnueLoadSource =
    | { type: "idb"; id: string } // IndexedDB から
    | { type: "url"; url: string } // URL から fetch
    | { type: "bytes"; bytes: Uint8Array }; // 直接バイト列（transferable）

type WorkerCommand =
    | InitCommand
    | (CommandBase & {
          type: "loadPosition";
          sfen: string;
          moves?: string[];
          passRights?: PassRightsInput;
      })
    | (CommandBase & { type: "applyMoves"; moves: string[] })
    | (CommandBase & { type: "search"; params: SearchParams })
    | (CommandBase & { type: "stop" })
    | (CommandBase & { type: "dispose" })
    | (CommandBase & { type: "setOption"; name: string; value: string | number | boolean })
    | (CommandBase & { type: "loadNnue"; source: NnueLoadSource });

type ModelCache = { uri: string; bytes: Uint8Array };

type AckMessage = { type: "ack"; requestId: string; error?: string };

const INFO_THROTTLE_MS = 50;

/**
 * Worker 内で IndexedDB から NNUE バイナリを読み込む
 * メモリ効率のため、事前確保方式で stream 読み込み
 */
async function loadNnueFromIndexedDB(
    id: string,
    onProgress: (loaded: number, total: number) => void,
): Promise<Uint8Array> {
    return new Promise((resolve, reject) => {
        const request = indexedDB.open(NNUE_DB_NAME, NNUE_DB_VERSION);

        request.onerror = () => reject(new Error(`IndexedDB open failed: ${request.error}`));

        request.onsuccess = () => {
            const db = request.result;
            const tx = db.transaction("nnue-blobs", "readonly");
            const store = tx.objectStore("nnue-blobs");
            const getRequest = store.get(id);

            getRequest.onerror = () => reject(new Error(`Failed to get NNUE: ${getRequest.error}`));

            getRequest.onsuccess = async () => {
                const blob = getRequest.result as Blob | undefined;
                if (!blob) {
                    reject(new Error(`NNUE not found: ${id}`));
                    return;
                }

                try {
                    // 事前にサイズが分かるので、最初から最終バッファを確保
                    const result = new Uint8Array(blob.size);
                    const reader = blob.stream().getReader();
                    let offset = 0;
                    let lastProgressTime = 0;

                    while (true) {
                        const { done, value } = await reader.read();
                        if (done) break;

                        // 逐次コピー（chunks配列を持たない）
                        result.set(value, offset);
                        offset += value.length;

                        // 進捗通知（スロットリング）
                        const now = Date.now();
                        if (now - lastProgressTime > NNUE_PROGRESS_THROTTLE_MS) {
                            onProgress(offset, blob.size);
                            lastProgressTime = now;
                        }
                    }

                    // 最終進捗を通知
                    onProgress(blob.size, blob.size);
                    resolve(result);
                } catch (error) {
                    reject(error);
                }
            };
        };

        request.onupgradeneeded = (event) => {
            // Worker から開く場合でも DB が存在しない可能性があるため、
            // indexed-db.ts と同じスキーマでストアを作成する
            const db = (event.target as IDBOpenDBRequest).result;
            if (!db.objectStoreNames.contains("nnue-blobs")) {
                db.createObjectStore("nnue-blobs");
            }
            if (!db.objectStoreNames.contains("nnue-meta")) {
                const metaStore = db.createObjectStore("nnue-meta", { keyPath: "id" });
                metaStore.createIndex("by-source", "source");
                metaStore.createIndex("by-created", "createdAt");
                metaStore.createIndex("by-preset-key", "presetKey");
                metaStore.createIndex("by-content-hash", "contentHashSha256");
            }
        };
    });
}

/**
 * URL から NNUE をストリーム読み込み
 */
async function loadNnueFromUrl(
    url: string,
    onProgress: (loaded: number, total: number) => void,
): Promise<Uint8Array> {
    const res = await fetch(url);
    if (!res.ok) {
        throw new Error(`Failed to fetch NNUE from ${url}: ${res.status} ${res.statusText}`);
    }

    const contentLength = res.headers.get("content-length");
    const total = contentLength ? Number.parseInt(contentLength, 10) : 0;

    if (!res.body) {
        // Fallback: body が ReadableStream でない場合
        const buffer = await res.arrayBuffer();
        onProgress(buffer.byteLength, buffer.byteLength);
        return new Uint8Array(buffer);
    }

    // Stream 読み込み
    const reader = res.body.getReader();
    const chunks: Uint8Array[] = [];
    let loaded = 0;
    let lastProgressTime = 0;

    while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        chunks.push(value);
        loaded += value.length;

        const now = Date.now();
        if (now - lastProgressTime > NNUE_PROGRESS_THROTTLE_MS) {
            onProgress(loaded, total);
            lastProgressTime = now;
        }
    }

    // チャンクを結合
    const result = new Uint8Array(loaded);
    let offset = 0;
    for (const chunk of chunks) {
        result.set(chunk, offset);
        offset += chunk.length;
    }

    onProgress(loaded, total);
    return result;
}

export function createEngineWorker(bindings: WasmWorkerBindings) {
    const ctx: {
        postMessage: (value: unknown) => void;
        onmessage: ((msg: { data: WorkerCommand }) => void) | null;
    } = self as unknown as {
        postMessage: (value: unknown) => void;
        onmessage: ((msg: { data: WorkerCommand }) => void) | null;
    };

    let engineInitialized = false;
    let cachedModel: ModelCache | null = null;
    let lastInit: InitCommand | null = null;
    let moduleReady: Promise<void> | null = null;
    let threadPoolReady: Promise<void> | null = null;

    let lastInfoPostedAt = Number.NEGATIVE_INFINITY;
    const pendingInfoByPv = new Map<number, EngineEvent>();
    let pendingEvents: EngineEvent[] = [];

    const getNowMs = () => (typeof performance !== "undefined" ? performance.now() : Date.now());

    const postAck = (requestId: string | undefined, error?: unknown) => {
        if (!requestId) return;
        const message: AckMessage = { type: "ack", requestId };
        if (error) {
            message.error = String(error);
        }
        ctx.postMessage(message);
    };

    const flushEvents = () => {
        const batch: EngineEvent[] = [];

        if (pendingInfoByPv.size) {
            const infos = Array.from(pendingInfoByPv.entries())
                .sort(([a], [b]) => a - b)
                .map(([, event]) => event);
            batch.push(...infos);
            pendingInfoByPv.clear();
        }

        if (pendingEvents.length) {
            batch.push(...pendingEvents);
            pendingEvents = [];
        }

        if (!batch.length) return;
        ctx.postMessage({ type: "events", payload: batch });
    };

    const postEvent = (event: EngineEvent) => {
        if (event.type === "info") {
            const pv = typeof event.multipv === "number" && event.multipv > 0 ? event.multipv : 1;
            pendingInfoByPv.set(pv, event);
            const now = getNowMs();
            if (now - lastInfoPostedAt >= INFO_THROTTLE_MS) {
                lastInfoPostedAt = now;
                flushEvents();
            }
            return;
        }
        // Force flush pending info before bestmove to ensure final stats are sent
        if (event.type === "bestmove" && pendingInfoByPv.size > 0) {
            flushEvents();
        }
        pendingEvents.push(event);
        flushEvents();
    };

    async function ensureModule(wasmModule?: WasmModuleSource) {
        if (!moduleReady) {
            const input =
                wasmModule ??
                bindings.defaultWasmModuleUrl ??
                new URL("../pkg/engine_wasm_bg.wasm", import.meta.url);
            moduleReady = bindings.initWasm({ module_or_path: input }).then(() => {
                bindings.setEventHandler((event: EngineEvent) => postEvent(event));
            });
        }
        await moduleReady;
    }

    async function ensureThreadPool(opts?: EngineInitOptions) {
        const threads = opts?.threads ?? 1;
        const poolSize = Math.max(0, threads - 1);
        if (poolSize <= 0) return;
        if (!bindings.initThreadPool) {
            throw new Error("initThreadPool missing in threaded build");
        }
        if (!threadPoolReady) {
            threadPoolReady = Promise.resolve(bindings.initThreadPool(poolSize)).then(
                () => undefined,
            );
        }
        await threadPoolReady;
    }

    async function loadModelIfNeeded(opts?: EngineInitOptions) {
        const uri = opts?.modelUri ?? opts?.nnuePath;
        if (!uri) return;

        if (cachedModel && cachedModel.uri === uri) {
            await bindings.loadModel(cachedModel.bytes);
            return;
        }

        const res = await fetch(uri);
        if (!res.ok) {
            throw new Error(`Failed to fetch model from ${uri}: ${res.status} ${res.statusText}`);
        }

        const bytes = new Uint8Array(await res.arrayBuffer());
        cachedModel = { uri, bytes };
        await bindings.loadModel(bytes);
    }

    async function applyInit(command?: InitCommand) {
        await ensureModule(command?.wasmModule ?? lastInit?.wasmModule);

        const opts = command?.opts ?? lastInit?.opts;
        await ensureThreadPool(opts);
        await bindings.initEngine(opts ?? undefined);
        engineInitialized = true;

        await loadModelIfNeeded(opts);
    }

    async function ensureEngineReady() {
        if (!engineInitialized) {
            await applyInit(lastInit ?? undefined);
            engineInitialized = true;
        }
    }

    /**
     * NNUE ロード進捗を通知
     */
    function postNnueProgress(loaded: number, total: number) {
        ctx.postMessage({
            type: "nnueLoadProgress",
            loaded,
            total,
        });
    }

    /**
     * NNUE をロード
     */
    async function handleLoadNnue(source: NnueLoadSource): Promise<void> {
        let bytes: Uint8Array;

        switch (source.type) {
            case "idb":
                bytes = await loadNnueFromIndexedDB(source.id, postNnueProgress);
                break;
            case "url":
                bytes = await loadNnueFromUrl(source.url, postNnueProgress);
                break;
            case "bytes":
                bytes = source.bytes;
                postNnueProgress(bytes.length, bytes.length);
                break;
        }

        // Wasm にロード
        await bindings.loadModel(bytes);

        // 成功通知
        ctx.postMessage({
            type: "nnueLoaded",
            size: bytes.length,
        });
    }

    let commandQueue: Promise<void> = Promise.resolve();

    async function handleCommand(command: WorkerCommand) {
        const requestId = command.requestId;

        try {
            switch (command.type) {
                case "init":
                    lastInit = command;
                    await applyInit(command);
                    postAck(requestId);
                    break;
                case "loadPosition":
                    await ensureEngineReady();
                    await bindings.loadPosition(
                        command.sfen,
                        command.moves ?? undefined,
                        command.passRights,
                    );
                    postAck(requestId);
                    break;
                case "applyMoves":
                    await ensureEngineReady();
                    await bindings.applyMoves(command.moves);
                    postAck(requestId);
                    break;
                case "search":
                    await ensureEngineReady();
                    lastInfoPostedAt = Number.NEGATIVE_INFINITY;
                    postAck(requestId);
                    await bindings.runSearch(command.params ?? undefined);
                    break;
                case "setOption":
                    await ensureEngineReady();
                    await bindings.setOption(command.name, command.value);
                    postAck(requestId);
                    break;
                case "stop":
                    if (engineInitialized) {
                        bindings.stopEngine();
                    }
                    postAck(requestId);
                    break;
                case "dispose":
                    if (engineInitialized) {
                        bindings.disposeEngine();
                        engineInitialized = false;
                    }
                    postAck(requestId);
                    break;
                case "loadNnue":
                    await ensureModule(lastInit?.wasmModule);
                    await handleLoadNnue(command.source);
                    postAck(requestId);
                    break;
                default:
                    postAck(requestId);
                    break;
            }
        } catch (error) {
            postAck(requestId, error);
            postEvent({ type: "error", message: String(error) });
        }
    }

    ctx.onmessage = (msg: { data: WorkerCommand }) => {
        const command = msg.data;
        commandQueue = commandQueue
            .then(() => handleCommand(command))
            .catch((error) => {
                postEvent({ type: "error", message: String(error) });
            });
    };
}
