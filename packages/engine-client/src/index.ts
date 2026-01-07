export type EngineBackend = "native" | "wasm" | "external-usi";

export type EngineStopMode = "terminate" | "cooperative";

// ============================================================
// Skill Level Settings
// ============================================================

/**
 * Skill Level 設定
 *
 * エンジンの強さを制御するための設定。
 * - skillLevel: 0-20 の整数（0=最弱、20=全力）
 */
export interface SkillLevelSettings {
    /** スキルレベル (0-20, 20=全力) */
    skillLevel: number;
}

/**
 * プリセットレベル
 */
export type SkillPreset = "beginner" | "intermediate" | "advanced" | "professional" | "custom";

/**
 * プリセットから SkillLevelSettings への変換マップ
 */
export const SKILL_PRESETS: Record<Exclude<SkillPreset, "custom">, SkillLevelSettings> = {
    beginner: { skillLevel: 2 },
    intermediate: { skillLevel: 10 },
    advanced: { skillLevel: 16 },
    professional: { skillLevel: 20 },
};

/** Skill Level の有効範囲 */
export const SKILL_LEVEL_MIN = 0;
export const SKILL_LEVEL_MAX = 20;

/**
 * SkillLevelSettings のバリデーション結果
 */
export interface SkillLevelValidationResult {
    valid: boolean;
    errors: string[];
}

/**
 * SkillLevelSettings をバリデーションする
 */
export function validateSkillLevelSettings(
    settings: SkillLevelSettings,
): SkillLevelValidationResult {
    const errors: string[] = [];

    if (settings.skillLevel < SKILL_LEVEL_MIN || settings.skillLevel > SKILL_LEVEL_MAX) {
        errors.push(
            `skillLevel must be between ${SKILL_LEVEL_MIN} and ${SKILL_LEVEL_MAX}, got ${settings.skillLevel}`,
        );
    }

    return {
        valid: errors.length === 0,
        errors,
    };
}

/**
 * SkillLevelSettings の値をクランプして正規化する
 */
export function normalizeSkillLevelSettings(settings: SkillLevelSettings): SkillLevelSettings {
    const skillLevel = Math.max(SKILL_LEVEL_MIN, Math.min(SKILL_LEVEL_MAX, settings.skillLevel));
    return { skillLevel };
}

/**
 * SkillLevelSettings からプリセットを推定
 */
export function detectSkillPreset(settings: SkillLevelSettings): SkillPreset {
    // 範囲外の値はカスタムとして扱う
    if (settings.skillLevel < SKILL_LEVEL_MIN || settings.skillLevel > SKILL_LEVEL_MAX) {
        return "custom";
    }

    for (const [preset, presetSettings] of Object.entries(SKILL_PRESETS) as [
        Exclude<SkillPreset, "custom">,
        SkillLevelSettings,
    ][]) {
        if (presetSettings.skillLevel === settings.skillLevel) {
            return preset;
        }
    }
    return "custom";
}

/**
 * プリセット名の日本語表示
 */
export const SKILL_PRESET_LABELS: Record<SkillPreset, string> = {
    beginner: "初心者",
    intermediate: "中級者",
    advanced: "上級者",
    professional: "全力",
    custom: "カスタム",
};

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

export type EngineErrorSeverity = "warning" | "error" | "fatal";

/**
 * Well-known error codes for type-safe error handling.
 *
 * - WASM_* : Wasm固有のエラー（初期化失敗、スレッド関連）
 * - General : 一般的なエラー（モデル読み込み、局面、探索）
 * - ENGINE_ERROR_STATE : エンジンがエラー状態のため操作が拒否されたことを示す
 *   （エラー原因ではなく、エラー状態にあることを通知するために使用）
 */
export type EngineErrorCode =
    // Wasm-specific errors (fatal/recoverable)
    | "WASM_INIT_FAILED" // 一般的な初期化失敗
    | "WASM_NETWORK_ERROR" // Wasmファイル取得失敗（ネットワーク）
    | "WASM_MEMORY_ERROR" // メモリ不足
    | "WASM_WORKER_SPAWN_ERROR" // Worker生成失敗
    | "WASM_INIT_TIMEOUT" // 初期化タイムアウト
    // Wasm-specific warnings
    | "WASM_THREADS_UNAVAILABLE"
    | "WASM_THREADS_CLAMPED"
    | "WASM_THREADS_INIT_FAILED"
    | "WASM_THREADS_DEFERRED"
    | "WASM_WORKER_FAILED"
    // General errors
    | "MODEL_LOAD_FAILED"
    | "POSITION_INVALID"
    | "SEARCH_FAILED"
    | "TIMEOUT"
    | "UNKNOWN"
    // Error state indicator (not a failure cause, but indicates engine is in error state)
    | "ENGINE_ERROR_STATE";

/**
 * エラーコードに対応するユーザー向け情報
 */
export interface EngineErrorInfo {
    /** ユーザー向けメッセージ */
    userMessage: string;
    /** 考えられる原因 */
    possibleCauses: string[];
    /** 対処法 */
    solutions: string[];
    /** リトライ可能か */
    canRetry: boolean;
}

/**
 * エラーコードからユーザー向け情報を取得
 */
export function getEngineErrorInfo(code: EngineErrorCode | undefined): EngineErrorInfo {
    switch (code) {
        case "WASM_NETWORK_ERROR":
            return {
                userMessage: "エンジンの読み込みに失敗しました",
                possibleCauses: ["ネットワーク接続が不安定", "サーバーに一時的な問題が発生"],
                solutions: [
                    "ネットワーク接続を確認してください",
                    "しばらく待ってから再試行してください",
                ],
                canRetry: true,
            };
        case "WASM_MEMORY_ERROR":
            return {
                userMessage: "メモリ不足でエンジンを起動できません",
                possibleCauses: ["ブラウザのメモリ使用量が多い", "他のタブやアプリがメモリを消費"],
                solutions: [
                    "不要なタブを閉じてメモリを解放してください",
                    "ブラウザを再起動してください",
                ],
                canRetry: true,
            };
        case "WASM_WORKER_SPAWN_ERROR":
            return {
                userMessage: "エンジンの起動に失敗しました",
                possibleCauses: ["ブラウザのWorker生成制限に到達", "ブラウザの一時的な問題"],
                solutions: ["他のタブを閉じて再試行してください", "ブラウザを再起動してください"],
                canRetry: true,
            };
        case "WASM_INIT_TIMEOUT":
            return {
                userMessage: "エンジンの起動に時間がかかっています",
                possibleCauses: ["デバイスの処理能力が不足", "他のアプリがリソースを消費"],
                solutions: ["しばらく待ってから再試行してください", "他のアプリを終了してください"],
                canRetry: true,
            };
        case "WASM_INIT_FAILED":
            return {
                userMessage: "エンジンの初期化に失敗しました",
                possibleCauses: ["ブラウザがWebAssemblyに非対応", "一時的なエラー"],
                solutions: ["最新版のブラウザをお使いください", "ページを再読み込みしてください"],
                canRetry: true,
            };
        case "WASM_THREADS_UNAVAILABLE":
            return {
                userMessage: "マルチスレッド機能が利用できません",
                possibleCauses: [
                    "ブラウザがSharedArrayBufferに非対応",
                    "セキュリティヘッダーが設定されていない",
                ],
                solutions: ["シングルスレッドモードで動作します"],
                canRetry: false,
            };
        case "WASM_THREADS_INIT_FAILED":
            return {
                userMessage: "マルチスレッド初期化に失敗しました",
                possibleCauses: ["メモリ不足", "ブラウザの制限"],
                solutions: ["シングルスレッドモードで再試行します"],
                canRetry: false,
            };
        case "ENGINE_ERROR_STATE":
            return {
                userMessage: "エンジンがエラー状態です",
                possibleCauses: ["前回の操作でエラーが発生"],
                solutions: ["再試行ボタンを押してエンジンを再起動してください"],
                canRetry: true,
            };
        default:
            return {
                userMessage: "予期しないエラーが発生しました",
                possibleCauses: ["不明なエラー"],
                solutions: [
                    "ページを再読み込みしてください",
                    "問題が続く場合はブラウザを再起動してください",
                ],
                canRetry: true,
            };
    }
}

/** Backend status for error state management */
export type EngineBackendStatus = "ready" | "error" | "mock";

export interface EngineErrorEvent {
    type: "error";
    message: string;
    severity?: EngineErrorSeverity;
    code?: EngineErrorCode;
}

export type EngineEvent = EngineInfoEvent | EngineBestMoveEvent | EngineErrorEvent;

export type EngineEventHandler = (event: EngineEvent) => void;

export interface SearchHandle {
    cancel(): Promise<void>;
}

/**
 * Thread information for debugging and monitoring parallel search.
 */
export interface ThreadInfo {
    /** Number of threads currently active (1 = single-threaded) */
    activeThreads: number;
    /** Maximum threads allowed (based on hardware and wasm limits) */
    maxThreads: number;
    /** Whether threaded execution is available (SharedArrayBuffer, crossOriginIsolated) */
    threadedAvailable: boolean;
    /** Hardware concurrency reported by navigator */
    hardwareConcurrency: number;
}

export interface EngineClient {
    init(opts?: EngineInitOptions): Promise<void>;
    loadPosition(sfen: string, moves?: string[]): Promise<void>;
    search(params: SearchParams): Promise<SearchHandle>;
    stop(): Promise<void>;
    setOption(name: string, value: string | number | boolean): Promise<void>;
    subscribe(handler: EngineEventHandler): () => void;
    dispose(): Promise<void>;
    /**
     * Get thread information for debugging parallel search.
     * Optional - may not be implemented by all backends.
     */
    getThreadInfo?(): ThreadInfo;
    /**
     * Reset the engine to allow retry after error.
     * - Clears error state and allows reinitialization
     * - Does NOT automatically call init() - caller must do so after reset
     * - Safe to call even when engine is not in error state (no-op)
     * - Terminates any existing worker and cancels pending operations
     * Optional - only implemented by wasm backend.
     */
    reset?(): Promise<void>;
    /**
     * Get current backend status.
     * Optional - only implemented by wasm backend.
     */
    getBackendStatus?(): EngineBackendStatus;
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
