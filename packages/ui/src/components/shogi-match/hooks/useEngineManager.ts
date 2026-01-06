import type { GameResult, Player, PositionState } from "@shogi/app-core";
import type {
    EngineClient,
    EngineErrorCode,
    EngineEvent,
    EngineInfoEvent,
    SearchHandle,
    SkillLevelSettings,
} from "@shogi/engine-client";
import { getEngineErrorInfo, normalizeSkillLevelSettings } from "@shogi/engine-client";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ClockSettings, TickState } from "./useClockManager";

type EngineStatus = "idle" | "thinking" | "error";

interface EngineErrorDetails {
    hasError: boolean;
    errorCode?: EngineErrorCode;
    errorMessage?: string;
    canRetry: boolean;
}

interface SearchState {
    handle: SearchHandle | null;
    pending: boolean;
    requestPly: number | null;
}

interface EngineState {
    client: EngineClient | null;
    subscription: (() => void) | null;
    selectedId: string | null;
    ready: boolean;
}

interface ActiveSearch {
    side: Player;
    engineId: string;
}

interface BestmoveHandlerParams {
    move: string;
    side: Player;
    engineId: string;
    activeSearch: ActiveSearch | null;
    movesCount: number;
}

interface BestmoveHandlerResult {
    action: "apply_move" | "end_match" | "skip";
    move?: string;
    gameResult?: GameResult;
    shouldClearActive: boolean;
    shouldUpdateRequestPly: boolean;
}

type EngineOption = {
    id: string;
    label: string;
    createClient: () => EngineClient;
    kind?: "internal" | "external";
};

type SideRole = "human" | "engine";

type SideSetting = {
    role: SideRole;
    engineId?: string;
    /** エンジンの強さ設定（role="engine"時のみ有効） */
    skillLevel?: SkillLevelSettings;
};

interface UseEngineManagerProps {
    /** 先手/後手の設定 */
    sides: { sente: SideSetting; gote: SideSetting };
    /** エンジンオプション */
    engineOptions: EngineOption[];
    /** 時間設定 */
    timeSettings: ClockSettings;
    /** 現在の時計状態への参照（リアルタイムの残り時間計算用） */
    clocksRef: { readonly current: TickState };
    /** 開始局面のSFEN */
    startSfen: string;
    /** 棋譜の ref */
    movesRef: { current: string[] };
    /** 局面の ref */
    positionRef: { current: PositionState };
    /** 対局実行中かどうか */
    isMatchRunning: boolean;
    /** 局面が準備完了しているか */
    positionReady: boolean;
    /** エンジンからの手を適用するコールバック */
    onMoveFromEngine: (move: string) => void;
    /** 対局終了時のコールバック */
    onMatchEnd: (result: GameResult) => Promise<void>;
    /** 評価値更新時のコールバック */
    onEvalUpdate?: (ply: number, event: EngineInfoEvent) => void;
    /** ログの最大件数 */
    maxLogs?: number;
}

/** 解析リクエストパラメータ */
interface AnalysisRequest {
    sfen: string;
    moves: string[];
    ply: number;
    /** 解析深さ（省略時はデフォルト15） */
    depth?: number;
    /** 解析時間制限（省略時は3秒） */
    timeMs?: number;
}

/** 解析のデフォルト設定 */
const DEFAULT_ANALYSIS_TIME_MS = 3000;
const DEFAULT_ANALYSIS_DEPTH = 15;

/**
 * エンジンに Skill Level 設定を適用する
 *
 * @throws エンジンへのオプション設定が失敗した場合
 */
async function applySkillLevelSettings(
    client: EngineClient,
    settings: SkillLevelSettings,
): Promise<void> {
    // 値を正規化（範囲外の値をクランプ）
    const normalized = normalizeSkillLevelSettings(settings);

    try {
        await client.setOption("Skill Level", normalized.skillLevel);

        if (normalized.useLimitStrength && normalized.elo !== undefined) {
            await client.setOption("UCI_LimitStrength", true);
            await client.setOption("UCI_Elo", normalized.elo);
        } else {
            await client.setOption("UCI_LimitStrength", false);
        }
    } catch (error) {
        const errorMsg = error instanceof Error ? error.message : String(error);
        throw new Error(
            `Failed to apply skill level settings (skillLevel: ${normalized.skillLevel}, elo: ${normalized.elo ?? "none"}): ${errorMsg}`,
        );
    }
}

interface UseEngineManagerReturn {
    /** エンジンの準備状態 */
    engineReady: Record<Player, boolean>;
    /** エンジンのステータス */
    engineStatus: Record<Player, EngineStatus>;
    /** イベントログ */
    eventLogs: string[];
    /** エラーログ */
    errorLogs: string[];
    /** 全エンジンを停止する */
    stopAllEngines: () => Promise<void>;
    /** 指定サイドのエンジンオプションを取得 */
    getEngineForSide: (side: Player) => EngineOption | undefined;
    /** 指定手番がエンジンかどうか */
    isEngineTurn: (turn: Player) => boolean;
    /** エンジンエラーログを追加する（親でバリデーションした結果の通知用） */
    logEngineError: (message: string) => void;
    /** 解析中かどうか */
    isAnalyzing: boolean;
    /** 局面を解析する（対局中でないときのみ利用可能） */
    analyzePosition: (request: AnalysisRequest) => Promise<void>;
    /** 解析をキャンセルする */
    cancelAnalysis: () => Promise<void>;
    /** エンジンエラーの詳細情報 */
    engineErrorDetails: Record<Player, EngineErrorDetails | null>;
    /** エンジンをリトライする */
    retryEngine: (side: Player) => Promise<void>;
    /** リトライ中かどうか */
    isRetrying: Record<Player, boolean>;
}

export function formatEvent(event: EngineEvent, label: string): string {
    if (event.type === "bestmove") {
        return `[${label}] bestmove ${event.move}`;
    }
    if (event.type === "info") {
        const parts: string[] = [`[${label}] info`];
        if (event.depth !== undefined) parts.push(`depth ${event.depth}`);
        if (event.seldepth !== undefined) parts.push(`seldepth ${event.seldepth}`);
        if (event.scoreCp !== undefined) parts.push(`score cp ${event.scoreCp}`);
        if (event.nodes !== undefined) parts.push(`nodes ${event.nodes}`);
        if (event.nps !== undefined) parts.push(`nps ${event.nps}`);
        if (event.pv && event.pv.length > 0) parts.push(`pv ${event.pv.join(" ")}`);
        return parts.join(" ");
    }
    if (event.type === "error") {
        return `[${label}] error: ${event.message}`;
    }
    return `[${label}] unknown event`;
}

export function determineBestmoveAction(params: BestmoveHandlerParams): BestmoveHandlerResult {
    const { move, side, engineId, activeSearch, movesCount } = params;

    // Active Searchのマッチング確認
    if (!activeSearch || activeSearch.engineId !== engineId || activeSearch.side !== side) {
        return {
            action: "skip",
            shouldClearActive: false,
            shouldUpdateRequestPly: false,
        };
    }

    // トークン処理
    const trimmed = move.trim();
    const token = trimmed.toLowerCase();

    // 勝者の計算
    const winner: Player = side === "sente" ? "gote" : "sente";

    switch (token) {
        case "win":
            return {
                action: "end_match",
                gameResult: {
                    winner: side,
                    reason: { kind: "win_declaration", winner: side },
                    totalMoves: movesCount,
                },
                shouldClearActive: true,
                shouldUpdateRequestPly: true,
            };
        case "resign":
            return {
                action: "end_match",
                gameResult: {
                    winner,
                    reason: { kind: "resignation", loser: side },
                    totalMoves: movesCount,
                },
                shouldClearActive: true,
                shouldUpdateRequestPly: true,
            };
        case "none":
            return {
                action: "end_match",
                gameResult: {
                    winner,
                    reason: { kind: "checkmate", loser: side },
                    totalMoves: movesCount,
                },
                shouldClearActive: true,
                shouldUpdateRequestPly: true,
            };
        default:
            return {
                action: "apply_move",
                move: trimmed,
                shouldClearActive: true,
                shouldUpdateRequestPly: true,
            };
    }
}

export function useEngineManager({
    sides,
    engineOptions,
    timeSettings: _timeSettings,
    clocksRef,
    startSfen,
    movesRef,
    positionRef,
    isMatchRunning,
    positionReady,
    onMoveFromEngine,
    onMatchEnd,
    onEvalUpdate,
    maxLogs = 80,
}: UseEngineManagerProps): UseEngineManagerReturn {
    const [engineReady, setEngineReady] = useState<Record<Player, boolean>>({
        sente: false,
        gote: false,
    });
    const [engineStatus, setEngineStatus] = useState<Record<Player, EngineStatus>>({
        sente: "idle",
        gote: "idle",
    });
    const [eventLogs, setEventLogs] = useState<string[]>([]);
    const [errorLogs, setErrorLogs] = useState<string[]>([]);
    const [isAnalyzing, setIsAnalyzing] = useState(false);
    const [engineErrorDetails, setEngineErrorDetails] = useState<
        Record<Player, EngineErrorDetails | null>
    >({
        sente: null,
        gote: null,
    });
    const [isRetrying, setIsRetrying] = useState<Record<Player, boolean>>({
        sente: false,
        gote: false,
    });

    const addErrorLog = useCallback(
        (message: string) => {
            setErrorLogs((prev) => [message, ...prev].slice(0, maxLogs));
        },
        [maxLogs],
    );

    const engineStatesRef = useRef<Record<Player, EngineState>>({
        sente: { client: null, subscription: null, selectedId: null, ready: false },
        gote: { client: null, subscription: null, selectedId: null, ready: false },
    });

    const searchStatesRef = useRef<Record<Player, SearchState>>({
        sente: { handle: null, pending: false, requestPly: null },
        gote: { handle: null, pending: false, requestPly: null },
    });

    const activeSearchRef = useRef<ActiveSearch | null>(null);
    const isMatchRunningRef = useRef(isMatchRunning);
    const initializingRef = useRef<Record<Player, boolean>>({
        sente: false,
        gote: false,
    });

    // 解析用エンジン状態
    const analysisEngineRef = useRef<{
        client: EngineClient | null;
        subscription: (() => void) | null;
        handle: SearchHandle | null;
        ply: number | null;
    }>({
        client: null,
        subscription: null,
        handle: null,
        ply: null,
    });

    const engineMap = useMemo(() => {
        const map = new Map<string, EngineOption>();
        for (const opt of engineOptions) {
            map.set(opt.id, opt);
        }
        return map;
    }, [engineOptions]);

    useEffect(() => {
        isMatchRunningRef.current = isMatchRunning;
    }, [isMatchRunning]);

    const getEngineForSide = useCallback(
        (side: Player): EngineOption | undefined => {
            const setting = sides[side];
            if (setting.role !== "engine") return undefined;
            const selectedId = setting.engineId ?? engineOptions[0]?.id;
            if (!selectedId) return undefined;
            return engineMap.get(selectedId);
        },
        [engineMap, engineOptions, sides],
    );

    const isEngineTurn = useCallback(
        (turn: Player): boolean => {
            return sides[turn].role === "engine";
        },
        [sides],
    );

    const disposeEngineForSide = useCallback(
        async (side: Player) => {
            const engineState = engineStatesRef.current[side];
            const searchState = searchStatesRef.current[side];

            try {
                if (searchState.handle) {
                    await searchState.handle.cancel();
                }
            } catch (error) {
                console.error(`Failed to cancel search for ${side}:`, error);
                addErrorLog(`検索キャンセルに失敗 (${side}): ${String(error)}`);
            } finally {
                searchState.handle = null;
                searchState.pending = false;
                searchState.requestPly = null;
                // activeSearchRefを無条件でクリア（条件判定を削除して堅牢化）
                activeSearchRef.current = null;
            }

            try {
                if (engineState.subscription) {
                    engineState.subscription();
                }
            } catch (error) {
                console.error(`Failed to unsubscribe engine for ${side}:`, error);
                addErrorLog(`サブスクリプション解除に失敗 (${side}): ${String(error)}`);
            } finally {
                engineState.subscription = null;
            }

            try {
                if (engineState.client) {
                    await engineState.client.stop();
                    if (typeof engineState.client.dispose === "function") {
                        await engineState.client.dispose();
                    }
                }
            } catch (error) {
                console.error(`Failed to dispose engine for ${side}:`, error);
                addErrorLog(`エンジン破棄に失敗 (${side}): ${String(error)}`);
            } finally {
                engineState.client = null;
            }

            engineState.selectedId = null;
            engineState.ready = false;

            setEngineReady((prev) => ({ ...prev, [side]: false }));
            setEngineStatus((prev) => ({ ...prev, [side]: "idle" }));
        },
        [addErrorLog],
    );

    const stopAllEngines = useCallback(async () => {
        await Promise.all(
            (["sente", "gote"] as Player[]).map((side) => disposeEngineForSide(side)),
        );
    }, [disposeEngineForSide]);

    const retryEngine = useCallback(
        async (side: Player) => {
            const engineState = engineStatesRef.current[side];
            if (!engineState.client) return;

            // Check pending state first, before setting isRetrying
            // This prevents isRetrying from getting stuck if we return early
            const searchState = searchStatesRef.current[side];
            if (searchState.pending) {
                addErrorLog(`リトライ中です (${side})`);
                return;
            }

            // Prevent concurrent retry attempts using React state
            setIsRetrying((prev) => {
                if (prev[side]) {
                    return prev;
                }
                return { ...prev, [side]: true };
            });

            searchState.pending = true;

            try {
                // Call reset() if the client supports it
                const client = engineState.client;
                if ("reset" in client && typeof client.reset === "function") {
                    await client.reset();
                }

                // Clear error state before retry
                setEngineErrorDetails((prev) => ({
                    ...prev,
                    [side]: null,
                }));
                setEngineStatus((prev) => ({ ...prev, [side]: "idle" }));
                engineState.ready = false;

                // Retry initialization
                await client.init();

                // Skill Level 設定の適用（リトライ時も再適用）
                const skillSettings = sides[side].skillLevel;
                if (skillSettings) {
                    await applySkillLevelSettings(client, skillSettings);
                }

                engineState.ready = true;
                setEngineReady((prev) => ({ ...prev, [side]: true }));
            } catch (error) {
                const errorMsg = error instanceof Error ? error.message : String(error);
                addErrorLog(`リトライ失敗 (${side}): ${errorMsg}`);
                setEngineStatus((prev) => ({ ...prev, [side]: "error" }));

                // Update error details on retry failure
                const errorInfo = getEngineErrorInfo("WASM_INIT_FAILED");
                setEngineErrorDetails((prev) => ({
                    ...prev,
                    [side]: {
                        hasError: true,
                        errorCode: "WASM_INIT_FAILED",
                        errorMessage: errorMsg,
                        canRetry: errorInfo.canRetry,
                    },
                }));
            } finally {
                searchState.pending = false;
                setIsRetrying((prev) => ({ ...prev, [side]: false }));
            }
        },
        [addErrorLog, sides],
    );

    const applyMoveFromEngine = useCallback(
        (move: string) => {
            const trimmed = move.trim();
            if (!trimmed) {
                addErrorLog("engine returned empty move");
                return;
            }
            onMoveFromEngine(trimmed);
        },
        [addErrorLog, onMoveFromEngine],
    );

    const attachSubscription = useCallback(
        (side: Player, client: EngineClient, engineId: string) => {
            const engineState = engineStatesRef.current[side];
            if (engineState.subscription) return;

            const unsub = client.subscribe((event) => {
                const label = `${side === "sente" ? "S" : "G"}:${engineId}`;
                setEventLogs((prev) => {
                    const next = [formatEvent(event, label), ...prev];
                    return next.length > maxLogs ? next.slice(0, maxLogs) : next;
                });
                if (event.type === "bestmove") {
                    const searchState = searchStatesRef.current[side];

                    // 対局終了後に届いたbestmoveは無視する
                    if (!isMatchRunningRef.current) {
                        searchState.pending = false;
                        searchState.handle = null;
                        searchState.requestPly = null;
                        if (activeSearchRef.current?.side === side) {
                            activeSearchRef.current = null;
                        }
                        setEngineStatus((prev) => ({ ...prev, [side]: "idle" }));
                        return;
                    }

                    // 状態のリセット
                    setEngineStatus((prev) => ({ ...prev, [side]: "idle" }));
                    searchState.pending = false;
                    searchState.handle = null;

                    // Bestmove処理ロジック
                    const result = determineBestmoveAction({
                        move: event.move,
                        side,
                        engineId,
                        activeSearch: activeSearchRef.current,
                        movesCount: movesRef.current.length,
                    });

                    // 結果に応じた副作用の実行
                    if (result.shouldClearActive) {
                        activeSearchRef.current = null;
                    }
                    if (result.shouldUpdateRequestPly) {
                        searchState.requestPly = movesRef.current.length;
                    }

                    switch (result.action) {
                        case "end_match":
                            if (result.gameResult) {
                                onMatchEnd(result.gameResult).catch((err) => {
                                    addErrorLog(`対局終了処理でエラー: ${String(err)}`);
                                });
                            }
                            break;
                        case "apply_move":
                            if (result.move) {
                                applyMoveFromEngine(result.move);
                            }
                            break;
                        case "skip":
                            // 何もしない（古い検索結果の無視）
                            break;
                    }
                }
                if (event.type === "info") {
                    // 評価値が含まれている場合はコールバックを呼ぶ
                    if (
                        onEvalUpdate &&
                        (event.scoreCp !== undefined || event.scoreMate !== undefined)
                    ) {
                        // 現在の手数（現在局面の評価値として記録）
                        const ply = movesRef.current.length;
                        onEvalUpdate(ply, event);
                    }
                }
                if (event.type === "error") {
                    const searchState = searchStatesRef.current[side];

                    setEngineStatus((prev) => ({ ...prev, [side]: "error" }));
                    searchState.handle = null;
                    searchState.pending = false;
                    searchState.requestPly = null;
                    if (activeSearchRef.current?.side === side) {
                        activeSearchRef.current = null;
                    }

                    addErrorLog(event.message);

                    // Save error details for UI display
                    const errorInfo = getEngineErrorInfo(event.code);
                    setEngineErrorDetails((prev) => ({
                        ...prev,
                        [side]: {
                            hasError: true,
                            errorCode: event.code,
                            errorMessage: event.message,
                            canRetry: errorInfo.canRetry,
                        },
                    }));
                }
            });

            engineState.subscription = unsub;
        },
        [addErrorLog, applyMoveFromEngine, maxLogs, movesRef, onEvalUpdate, onMatchEnd],
    );

    const ensureEngineReady = useCallback(
        async (side: Player): Promise<{ client: EngineClient; engineId: string } | null> => {
            const setting = sides[side];
            if (setting.role !== "engine") return null;
            const selectedId = setting.engineId ?? engineOptions[0]?.id;
            if (!selectedId) return null;
            const opt = engineMap.get(selectedId);
            if (!opt) return null;

            if (initializingRef.current[side]) {
                return null;
            }
            initializingRef.current[side] = true;

            const engineState = engineStatesRef.current[side];

            try {
                // エンジンが変更された場合は既存のエンジンを破棄
                if (engineState.selectedId && engineState.selectedId !== opt.id) {
                    await disposeEngineForSide(side);
                }

                // エンジンクライアントの取得または作成
                let client = engineState.client;
                if (!client) {
                    client = opt.createClient();
                    engineState.client = client;
                    engineState.selectedId = opt.id;
                    engineState.ready = false;
                }

                // サブスクリプションのアタッチ
                attachSubscription(side, client, opt.id);

                // エンジンの初期化と局面読み込み
                if (!engineState.ready) {
                    await client.init();

                    // Skill Level 設定の適用
                    const skillSettings = setting.skillLevel;
                    if (skillSettings) {
                        await applySkillLevelSettings(client, skillSettings);
                    }

                    await client.loadPosition(startSfen, movesRef.current);
                    engineState.ready = true;
                    setEngineReady((prev) => ({ ...prev, [side]: true }));
                }

                return { client, engineId: opt.id };
            } finally {
                initializingRef.current[side] = false;
            }
        },
        [
            attachSubscription,
            disposeEngineForSide,
            engineMap,
            engineOptions,
            movesRef,
            sides,
            startSfen,
        ],
    );

    const startEngineTurn = useCallback(
        async (side: Player) => {
            if (!positionReady) return;

            const searchState = searchStatesRef.current[side];

            // 既に検索リクエストが送信待ちの場合はスキップ
            if (searchState.pending) return;

            const ready = await ensureEngineReady(side);
            if (!ready) return;
            const { client, engineId } = ready;

            // 既存の検索ハンドルがある場合の処理
            if (searchState.handle) {
                const current = activeSearchRef.current;
                if (current && current.side === side && current.engineId === engineId) {
                    return;
                }
                await searchState.handle.cancel().catch(() => undefined);
            }

            setEngineStatus((prev) => ({ ...prev, [side]: "thinking" }));
            searchState.pending = true;

            try {
                await client.loadPosition(startSfen, movesRef.current);

                // UIタイマーの現在の残り時間を計算してエンジンに渡す
                // これにより、タイマー開始からloadPosition完了までの経過時間を考慮できる
                const clocks = clocksRef.current;
                const clockState = clocks[side];
                const elapsedSinceUpdate = Date.now() - clocks.lastUpdatedAt;
                const remainingMainMs = Math.max(0, clockState.mainMs - elapsedSinceUpdate);
                let remainingByoyomiMs = clockState.byoyomiMs;

                // 持ち時間が消費された場合は秒読みから減らす
                if (remainingMainMs <= 0 && clockState.mainMs > 0) {
                    const overTime = elapsedSinceUpdate - clockState.mainMs;
                    remainingByoyomiMs = Math.max(0, clockState.byoyomiMs - overTime);
                } else if (clockState.mainMs === 0) {
                    // 持ち時間なしの秒読みモード
                    remainingByoyomiMs = Math.max(0, clockState.byoyomiMs - elapsedSinceUpdate);
                }

                // 最小100msを確保
                const effectiveByoyomiMs = Math.max(100, remainingByoyomiMs);

                const handle = await client.search({
                    limits: { byoyomiMs: effectiveByoyomiMs },
                    ponder: false,
                });

                searchState.handle = handle;
                activeSearchRef.current = { side, engineId };
            } finally {
                searchState.pending = false;
            }
        },
        [clocksRef, ensureEngineReady, movesRef, positionReady, startSfen],
    );

    // エンジンのrole変更時に破棄
    useEffect(() => {
        for (const side of ["sente", "gote"] as Player[]) {
            if (sides[side].role === "engine") continue;
            disposeEngineForSide(side).catch((error) => {
                console.error(`Failed to dispose engine on role change for ${side}:`, error);
                addErrorLog(`エンジン破棄に失敗 (${side}): ${String(error)}`);
            });
        }
    }, [addErrorLog, disposeEngineForSide, sides]);

    // アンマウント時に全エンジンを破棄
    useEffect(() => {
        return () => {
            Promise.all(
                (["sente", "gote"] as Player[]).map((side) => disposeEngineForSide(side)),
            ).catch((error) => {
                console.error("Failed to dispose engines on unmount:", error);
                addErrorLog(`エンジン破棄に失敗 (unmount): ${String(error)}`);
            });
        };
    }, [addErrorLog, disposeEngineForSide]);

    // エンジンターンの自動開始
    useEffect(() => {
        if (!isMatchRunning || !positionReady) return;
        const side = positionRef.current.turn;
        if (!isEngineTurn(side)) return;
        const engineOpt = getEngineForSide(side);
        if (!engineOpt) return;

        const searchState = searchStatesRef.current[side];
        const current = activeSearchRef.current;

        if (current && current.side === side && current.engineId === engineOpt.id) {
            return;
        }
        if (searchState.requestPly === movesRef.current.length) return;

        searchState.requestPly = movesRef.current.length;

        startEngineTurn(side).catch((error) => {
            setEngineStatus((prev) => ({ ...prev, [side]: "error" }));
            addErrorLog(`engine error: ${String(error)}`);
        });
    }, [
        addErrorLog,
        getEngineForSide,
        isEngineTurn,
        isMatchRunning,
        movesRef,
        positionReady,
        positionRef,
        startEngineTurn,
    ]);

    // 解析をキャンセルする
    const cancelAnalysis = useCallback(async () => {
        const analysisState = analysisEngineRef.current;
        try {
            if (analysisState.handle) {
                await analysisState.handle.cancel();
            }
        } catch (error) {
            console.error("Failed to cancel analysis:", error);
        } finally {
            analysisState.handle = null;
            analysisState.ply = null;
            setIsAnalyzing(false);
        }
    }, []);

    // 解析用エンジンを破棄する
    const disposeAnalysisEngine = useCallback(async () => {
        const analysisState = analysisEngineRef.current;

        // まず解析をキャンセル
        await cancelAnalysis();

        // サブスクリプションを解除
        if (analysisState.subscription) {
            analysisState.subscription();
            analysisState.subscription = null;
        }

        // エンジンを停止・破棄
        if (analysisState.client) {
            try {
                await analysisState.client.stop();
                if (typeof analysisState.client.dispose === "function") {
                    await analysisState.client.dispose();
                }
            } catch (error) {
                console.error("Failed to dispose analysis engine:", error);
            }
            analysisState.client = null;
        }
    }, [cancelAnalysis]);

    // 局面を解析する
    const analyzePosition = useCallback(
        async (request: AnalysisRequest) => {
            // 対局中は解析不可
            if (isMatchRunning) {
                addErrorLog("対局中は解析できません");
                return;
            }

            // 既に解析中の場合はキャンセル
            if (isAnalyzing) {
                await cancelAnalysis();
            }

            // 使用するエンジンを決定（対局で使用中のエンジンを優先）
            const engineOpt =
                engineOptions.find(
                    (opt) => opt.id === sides.sente.engineId || opt.id === sides.gote.engineId,
                ) ?? engineOptions[0];
            if (!engineOpt) {
                addErrorLog("利用可能なエンジンがありません");
                return;
            }

            const analysisState = analysisEngineRef.current;

            // 状態を初期化
            setIsAnalyzing(true);
            analysisState.ply = request.ply;

            // エンジンクライアントを作成または再利用
            let client = analysisState.client;
            if (!client) {
                try {
                    client = engineOpt.createClient();
                    analysisState.client = client;
                    await client.init();
                } catch (error) {
                    addErrorLog(`エンジン初期化エラー: ${String(error)}`);
                    analysisState.ply = null;
                    setIsAnalyzing(false);
                    return;
                }
            }

            // 既存のサブスクリプションがある場合は解除して再登録
            if (analysisState.subscription) {
                analysisState.subscription();
            }

            const unsub = client.subscribe((event) => {
                const label = "Analysis";
                setEventLogs((prev) => {
                    const next = [formatEvent(event, label), ...prev];
                    return next.length > maxLogs ? next.slice(0, maxLogs) : next;
                });

                if (event.type === "info") {
                    // 評価値が含まれている場合はコールバックを呼ぶ
                    if (
                        onEvalUpdate &&
                        (event.scoreCp !== undefined || event.scoreMate !== undefined)
                    ) {
                        const ply = analysisEngineRef.current.ply;
                        if (ply !== null) {
                            onEvalUpdate(ply, event);
                        }
                    }
                }

                if (event.type === "bestmove") {
                    // 解析完了
                    analysisEngineRef.current.handle = null;
                    analysisEngineRef.current.ply = null;
                    setIsAnalyzing(false);
                }

                if (event.type === "error") {
                    addErrorLog(event.message);
                    analysisEngineRef.current.handle = null;
                    analysisEngineRef.current.ply = null;
                    setIsAnalyzing(false);
                }
            });
            analysisState.subscription = unsub;

            // 局面を読み込み
            try {
                await client.loadPosition(request.sfen, request.moves);
            } catch (error) {
                addErrorLog(`局面読み込みエラー: ${String(error)}`);
                analysisState.ply = null;
                setIsAnalyzing(false);
                return;
            }

            // 探索開始
            try {
                const timeMs = request.timeMs ?? DEFAULT_ANALYSIS_TIME_MS;
                const depth = request.depth ?? DEFAULT_ANALYSIS_DEPTH;
                const handle = await client.search({
                    limits: {
                        movetimeMs: timeMs,
                        maxDepth: depth,
                    },
                    ponder: false,
                });

                analysisState.handle = handle;
            } catch (error) {
                addErrorLog(`探索開始エラー: ${String(error)}`);
                analysisState.ply = null;
                setIsAnalyzing(false);
            }
        },
        [
            addErrorLog,
            cancelAnalysis,
            engineOptions,
            isAnalyzing,
            isMatchRunning,
            maxLogs,
            onEvalUpdate,
            sides,
        ],
    );

    // アンマウント時に解析エンジンも破棄
    useEffect(() => {
        return () => {
            disposeAnalysisEngine().catch((error) => {
                console.error("Failed to dispose analysis engine on unmount:", error);
            });
        };
    }, [disposeAnalysisEngine]);

    return {
        engineReady,
        engineStatus,
        eventLogs,
        errorLogs,
        stopAllEngines,
        getEngineForSide,
        isEngineTurn,
        logEngineError: addErrorLog,
        isAnalyzing,
        analyzePosition,
        cancelAnalysis,
        engineErrorDetails,
        retryEngine,
        isRetrying,
    };
}
