export type EngineBackend = "native" | "wasm" | "external-usi";

export type EngineStopMode = "terminate" | "cooperative";

export interface EngineInitOptions {
    /** バックエンドの種類 (native/wasm/external-usi) */
    backend?: EngineBackend;
    /** 並列設定 (native / wasm の threaded build で使用) */
    threads?: number;
    /** Worker 数 (将来の並列用) */
    workers?: number;
    /** 停止モード: terminate または cooperative */
    stopMode?: EngineStopMode;
    /** NNUE/モデルのパス/URI */
    nnuePath?: string;
    modelUri?: string;
    /** 定跡パス */
    bookPath?: string;
    /** トランスポジションテーブルサイズ (MB) */
    ttSizeMb?: number;
    /** マルチPVの出力本数 */
    multiPv?: number;
}

export interface SearchLimits {
    /** 探索最大深さ */
    maxDepth?: number;
    /** ノード数上限 */
    nodes?: number;
    /** 秒読み (ms) */
    byoyomiMs?: number;
    /** 固定消費時間 (ms) */
    movetimeMs?: number;
}

export interface SearchParams {
    /** 探索条件 */
    limits?: SearchLimits;
    /** 先読みモード */
    ponder?: boolean;
}

export interface EngineInfoEvent {
    type: "info";
    depth?: number;
    seldepth?: number;
    /** 評価値 (センチポーン) */
    scoreCp?: number;
    /** メイトスコア (手数) */
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
    severity?: "warning" | "error";
    code?: string;
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
    let activeTimeout: ReturnType<typeof setTimeout> | null = null;

    const emit = (event: EngineEvent) => {
        for (const listener of listeners) {
            listener(event);
        }
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
                if (activeTimeout) {
                    clearTimeout(activeTimeout);
                    activeTimeout = null;
                }
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

            activeTimeout = setTimeout(() => {
                if (handle.cancelled) return;
                emit({ type: "bestmove", move: "resign" });
                activeHandle = null;
                activeTimeout = null;
            }, 50);

            return {
                async cancel() {
                    handle.cancelled = true;
                    if (activeTimeout) {
                        clearTimeout(activeTimeout);
                        activeTimeout = null;
                    }
                    activeHandle = null;
                },
            };
        },
        async stop() {
            if (activeHandle) {
                activeHandle.cancelled = true;
                activeHandle = null;
                if (activeTimeout) {
                    clearTimeout(activeTimeout);
                    activeTimeout = null;
                }
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
                if (activeTimeout) {
                    clearTimeout(activeTimeout);
                    activeTimeout = null;
                }
            }
        },
    };
}
