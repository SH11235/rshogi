import {
    EngineClient,
    EngineEventHandler,
    EngineInitOptions,
    EngineStopMode,
    SearchHandle,
    SearchParams,
    createMockEngineClient,
} from "@shogi/engine-client";

export interface WasmEngineInitOptions extends EngineInitOptions {
    /**
     * Optional preloaded wasm module or URL. When omitted, the client is expected to fetch it.
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

/**
 * Placeholder Wasm client. Currently delegates to the in-memory mock.
 * Laterこの場所で wasm-bindgen 出力 + Worker/terminate/chunk 停止戦略を隠蔽する。
 */
export function createWasmEngineClient(_opts: WasmEngineClientOptions = {}): EngineClient {
    return createMockEngineClient();
}
