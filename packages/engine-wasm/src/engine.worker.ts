import type { EngineEvent, EngineInitOptions, SearchParams } from "@shogi/engine-client";
import initWasm, {
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

type InitCommand = { type: "init"; opts?: EngineInitOptions; wasmModule?: WasmModuleSource };

type WorkerCommand =
    | InitCommand
    | { type: "loadPosition"; sfen: string; moves?: string[] }
    | { type: "search"; params: SearchParams }
    | { type: "stop" }
    | { type: "dispose" }
    | { type: "setOption"; name: string; value: string | number | boolean };

type ModelCache = { uri: string; bytes: Uint8Array };

const ctx: {
    postMessage: (value: unknown) => void;
    onmessage: ((msg: { data: WorkerCommand }) => void) | null;
} = self as any;

let engineInitialized = false;
let cachedModel: ModelCache | null = null;
let lastInit: InitCommand | null = null;
let moduleReady: Promise<void> | null = null;

const postEvent = (event: EngineEvent) => {
    ctx.postMessage({ type: "event", payload: event });
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

function buildInitPayload(opts?: EngineInitOptions) {
    if (!opts) return undefined;
    const payload: Record<string, unknown> = {};
    const typed = opts as { ttSizeMb?: number; multiPv?: number };
    if (typeof typed.ttSizeMb === "number") {
        payload.tt_size_mb = typed.ttSizeMb;
    }
    if (typeof typed.multiPv === "number") {
        payload.multi_pv = typed.multiPv;
    }
    return Object.keys(payload).length ? payload : undefined;
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

    const payload = buildInitPayload(command?.opts ?? lastInit?.opts);
    await initEngine(payload ? JSON.stringify(payload) : undefined);
    engineInitialized = true;

    await loadModelIfNeeded(command?.opts ?? lastInit?.opts);
}

async function ensureEngineReady() {
    if (!engineInitialized) {
        await applyInit(lastInit ?? undefined);
        engineInitialized = true;
    }
}

ctx.onmessage = async (msg: { data: WorkerCommand }) => {
    const command = msg.data;

    try {
        switch (command.type) {
            case "init":
                lastInit = command;
                await applyInit(command);
                break;
            case "loadPosition":
                await ensureEngineReady();
                await loadPosition(
                    command.sfen,
                    command.moves ? JSON.stringify(command.moves) : undefined,
                );
                break;
            case "search":
                await ensureEngineReady();
                await runSearch(JSON.stringify(command.params ?? {}));
                break;
            case "setOption":
                await ensureEngineReady();
                await setOption(command.name, JSON.stringify(command.value));
                break;
            case "stop":
                if (engineInitialized) {
                    stopEngine();
                }
                break;
            case "dispose":
                if (engineInitialized) {
                    disposeEngine();
                    engineInitialized = false;
                }
                break;
            default:
                break;
        }
    } catch (error) {
        postEvent({ type: "error", message: String(error) });
    }
};
