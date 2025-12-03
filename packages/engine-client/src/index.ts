export type EngineBackend = "native" | "wasm" | "external-usi";

export type EngineStopMode = "terminate" | "cooperative";

export interface EngineInitOptions {
    backend?: EngineBackend;
    threads?: number;
    workers?: number;
    stopMode?: EngineStopMode;
    nnuePath?: string;
    modelUri?: string;
    bookPath?: string;
}

export interface SearchLimits {
    maxDepth?: number;
    nodes?: number;
    byoyomiMs?: number;
    movetimeMs?: number;
}

export interface SearchParams {
    limits?: SearchLimits;
    ponder?: boolean;
}

export interface EngineInfoEvent {
    type: "info";
    depth?: number;
    seldepth?: number;
    scoreCp?: number;
    scoreMate?: number;
    nodes?: number;
    nps?: number;
    timeMs?: number;
    multipv?: number;
    pv?: string[];
    hashfull?: number;
}

export interface EngineBestMoveEvent {
    type: "bestmove";
    move: string;
    ponder?: string;
}

export interface EngineErrorEvent {
    type: "error";
    message: string;
}

export type EngineEvent = EngineInfoEvent | EngineBestMoveEvent | EngineErrorEvent;

export type EngineEventHandler = (event: EngineEvent) => void;

export interface SearchHandle {
    cancel(): Promise<void>;
}

export interface EngineClient {
    init(opts?: EngineInitOptions): Promise<void>;
    loadPosition(sfen: string, moves?: string[]): Promise<void>;
    search(params: SearchParams): Promise<SearchHandle>;
    stop(): Promise<void>;
    setOption(name: string, value: string | number | boolean): Promise<void>;
    subscribe(handler: EngineEventHandler): () => void;
    dispose(): Promise<void>;
}

/**
 * Simple in-memory mock that emits a single bestmove.
 * Useful for wiring UI before the real backends (Wasm/Tauri) are ready.
 */
export function createMockEngineClient(): EngineClient {
    const listeners = new Set<EngineEventHandler>();
    let activeHandle: { cancelled: boolean } | null = null;

    const emit = (event: EngineEvent) => {
        listeners.forEach((listener) => listener(event));
    };

    return {
        async init() {
            return;
        },
        async loadPosition() {
            return;
        },
        async search(): Promise<SearchHandle> {
            if (activeHandle) {
                activeHandle.cancelled = true;
            }
            const handle = { cancelled: false };
            activeHandle = handle;

            // Emit a tiny info then a bestmove after a short delay.
            setTimeout(() => {
                if (handle.cancelled) return;
                emit({
                    type: "info",
                    depth: 1,
                    scoreCp: 0,
                    nodes: 128,
                    nps: 1024,
                    pv: [],
                });
            }, 10);

            const timeout = setTimeout(() => {
                if (handle.cancelled) return;
                emit({ type: "bestmove", move: "resign" });
                activeHandle = null;
            }, 50);

            return {
                async cancel() {
                    handle.cancelled = true;
                    clearTimeout(timeout);
                    activeHandle = null;
                },
            };
        },
        async stop() {
            if (activeHandle) {
                activeHandle.cancelled = true;
                activeHandle = null;
            }
        },
        async setOption() {
            return;
        },
        subscribe(handler: EngineEventHandler) {
            listeners.add(handler);
            return () => listeners.delete(handler);
        },
        async dispose() {
            listeners.clear();
            if (activeHandle) {
                activeHandle.cancelled = true;
                activeHandle = null;
            }
        },
    };
}
