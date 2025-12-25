import type { Player, PositionState } from "@shogi/app-core";
import type {
    EngineClient,
    EngineEvent,
    EngineInfoEvent,
    SearchHandle,
} from "@shogi/engine-client";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ClockSettings } from "./useClockManager";

type EngineStatus = "idle" | "thinking" | "error";

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
    message?: string;
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
};

interface UseEngineManagerProps {
    /** 先手/後手の設定 */
    sides: { sente: SideSetting; gote: SideSetting };
    /** エンジンオプション */
    engineOptions: EngineOption[];
    /** 時間設定 */
    timeSettings: ClockSettings;
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
    onMatchEnd: (message: string) => Promise<void>;
    /** 評価値更新時のコールバック */
    onEvalUpdate?: (ply: number, event: EngineInfoEvent) => void;
    /** ログの最大件数 */
    maxLogs?: number;
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
    const { move, side, engineId, activeSearch } = params;

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

    // 特殊メッセージの確認
    const sideLabel = side === "sente" ? "先手" : "後手";
    const opponentLabel = side === "sente" ? "後手" : "先手";

    switch (token) {
        case "win":
            return {
                action: "end_match",
                message: `対局終了: ${sideLabel}が勝利宣言しました（win）。`,
                shouldClearActive: true,
                shouldUpdateRequestPly: true,
            };
        case "resign":
            return {
                action: "end_match",
                message: `対局終了: ${sideLabel}が投了しました（resign）。${opponentLabel}の勝ち。`,
                shouldClearActive: true,
                shouldUpdateRequestPly: true,
            };
        case "none":
            return {
                action: "end_match",
                message: `対局終了: ${sideLabel}が合法手なし（bestmove none）。${opponentLabel}の勝ち。`,
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
    timeSettings,
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
                            if (result.message) {
                                onMatchEnd(result.message).catch((err) => {
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
                        // 現在の手数+1（次の手の評価値として記録）
                        const ply = movesRef.current.length + 1;
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
                const handle = await client.search({
                    limits: { byoyomiMs: timeSettings[side].byoyomiMs },
                    ponder: false,
                });

                searchState.handle = handle;
                activeSearchRef.current = { side, engineId };
            } finally {
                searchState.pending = false;
            }
        },
        [ensureEngineReady, movesRef, positionReady, startSfen, timeSettings],
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

    return {
        engineReady,
        engineStatus,
        eventLogs,
        errorLogs,
        stopAllEngines,
        getEngineForSide,
        isEngineTurn,
        logEngineError: addErrorLog,
    };
}
