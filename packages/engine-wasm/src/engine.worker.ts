import type { EngineEvent, EngineInitOptions, SearchParams } from "@shogi/engine-client";
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
} from "../pkg/engine_wasm.js";

type WasmModuleSource = WebAssembly.Module | ArrayBuffer | Uint8Array | string;

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

const INFO_THROTTLE_MS = 50;

let lastInfoPostedAt = Number.NEGATIVE_INFINITY;
const pendingInfoByPv = new Map<number, EngineEvent>();
let pendingEvents: EngineEvent[] = [];

const getNowMs = () => (typeof performance !== "undefined" ? performance.now() : Date.now());

type AckMessage = { type: "ack"; requestId: string; error?: string };

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
        const input = wasmModule ?? new URL("../pkg/engine_wasm_bg.wasm", import.meta.url);
        moduleReady = initWasm({ module_or_path: input }).then(() => {
            setEventHandler((event: EngineEvent) => postEvent(event));
        });
    }
    await moduleReady;
}

async function loadModelIfNeeded(opts?: EngineInitOptions) {
    const uri = opts?.modelUri ?? opts?.nnuePath;
    if (!uri) return;

    if (cachedModel && cachedModel.uri === uri) {
        await loadModel(cachedModel.bytes);
        return;
    }

    const res = await fetch(uri);
    if (!res.ok) {
        throw new Error(`Failed to fetch model from ${uri}: ${res.status} ${res.statusText}`);
    }

    const bytes = new Uint8Array(await res.arrayBuffer());
    cachedModel = { uri, bytes };
    await loadModel(bytes);
}

async function applyInit(command?: InitCommand) {
    await ensureModule(command?.wasmModule ?? lastInit?.wasmModule);

    const opts = command?.opts ?? lastInit?.opts;
    await initEngine(opts ?? undefined);
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
                await loadPosition(command.sfen, command.moves ?? undefined);
                postAck(requestId);
                break;
            case "applyMoves":
                await ensureEngineReady();
                await applyMoves(command.moves);
                postAck(requestId);
                break;
            case "search":
                await ensureEngineReady();
                lastInfoPostedAt = Number.NEGATIVE_INFINITY;
                postAck(requestId);
                await runSearch(command.params ?? undefined);
                break;
            case "setOption":
                await ensureEngineReady();
                await setOption(command.name, command.value);
                postAck(requestId);
                break;
            case "stop":
                if (engineInitialized) {
                    stopEngine();
                }
                postAck(requestId);
                break;
            case "dispose":
                if (engineInitialized) {
                    disposeEngine();
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
