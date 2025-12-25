import type { EngineEvent, EngineInitOptions, SearchParams } from "@shogi/engine-client";

type WasmModuleSource = WebAssembly.Module | ArrayBuffer | Uint8Array | string | URL;
type WasmInitInput =
    | WasmModuleSource
    | {
          module_or_path: WasmModuleSource;
          memory?: WebAssembly.Memory;
          thread_stack_size?: number;
      };

type WasmWorkerBindings = {
    initWasm: (input?: WasmInitInput) => Promise<unknown>;
    applyMoves: (moves: string[]) => Promise<void> | void;
    disposeEngine: () => void;
    initEngine: (opts?: EngineInitOptions) => Promise<void> | void;
    loadModel: (bytes: Uint8Array) => Promise<void> | void;
    loadPosition: (sfen: string, moves?: string[]) => Promise<void> | void;
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

type WorkerCommand =
    | InitCommand
    | (CommandBase & { type: "loadPosition"; sfen: string; moves?: string[] })
    | (CommandBase & { type: "applyMoves"; moves: string[] })
    | (CommandBase & { type: "search"; params: SearchParams })
    | (CommandBase & { type: "stop" })
    | (CommandBase & { type: "dispose" })
    | (CommandBase & { type: "setOption"; name: string; value: string | number | boolean });

type ModelCache = { uri: string; bytes: Uint8Array };

type AckMessage = { type: "ack"; requestId: string; error?: string };

const INFO_THROTTLE_MS = 50;

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
                    await bindings.loadPosition(command.sfen, command.moves ?? undefined);
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
