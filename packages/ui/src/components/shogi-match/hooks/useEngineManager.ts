import type { Player, PositionState } from "@shogi/app-core";
import { applyMoveWithState } from "@shogi/app-core";
import type { EngineClient, EngineEvent, SearchHandle } from "@shogi/engine-client";
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

export type EngineOption = {
    id: string;
    label: string;
    createClient: () => EngineClient;
    kind?: "internal" | "external";
};

export type SideRole = "human" | "engine";

export type SideSetting = {
    role: SideRole;
    engineId?: string;
};

export interface UseEngineManagerProps {
    /** 先手/後手の設定 */
    sides: { sente: SideSetting; gote: SideSetting };
    /** エンジンオプション */
    engineOptions: EngineOption[];
    /** 時間設定 */
    timeSettings: ClockSettings;
    /** 開始局面のSFEN */
    startSfen: string;
    /** 棋譜の ref */
    movesRef: React.MutableRefObject<string[]>;
    /** 局面の ref */
    positionRef: React.MutableRefObject<PositionState>;
    /** 対局実行中かどうか */
    isMatchRunning: boolean;
    /** 局面が準備完了しているか */
    positionReady: boolean;
    /** エンジンからの手を適用するコールバック */
    onMoveFromEngine: (move: string) => void;
    /** 対局終了時のコールバック */
    onMatchEnd: (message: string) => Promise<void>;
    /** ログの最大件数 */
    maxLogs?: number;
}

export interface UseEngineManagerReturn {
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

    const engineStatesRef = useRef<Record<Player, EngineState>>({
        sente: { client: null, subscription: null, selectedId: null, ready: false },
        gote: { client: null, subscription: null, selectedId: null, ready: false },
    });

    const searchStatesRef = useRef<Record<Player, SearchState>>({
        sente: { handle: null, pending: false, requestPly: null },
        gote: { handle: null, pending: false, requestPly: null },
    });

    const activeSearchRef = useRef<ActiveSearch | null>(null);

    const engineMap = useMemo(() => {
        const map = new Map<string, EngineOption>();
        for (const opt of engineOptions) {
            map.set(opt.id, opt);
        }
        return map;
    }, [engineOptions]);

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

    const disposeEngineForSide = useCallback(async (side: Player) => {
        const engineState = engineStatesRef.current[side];
        const searchState = searchStatesRef.current[side];

        if (searchState.handle) {
            await searchState.handle.cancel().catch(() => undefined);
            searchState.handle = null;
        }

        if (engineState.subscription) {
            engineState.subscription();
            engineState.subscription = null;
        }

        if (engineState.client) {
            await engineState.client.stop().catch(() => undefined);
            if (typeof engineState.client.dispose === "function") {
                await engineState.client.dispose().catch(() => undefined);
            }
            engineState.client = null;
        }

        engineState.selectedId = null;
        engineState.ready = false;
        searchState.pending = false;
        searchState.requestPly = null;

        setEngineReady((prev) => ({ ...prev, [side]: false }));
        setEngineStatus((prev) => ({ ...prev, [side]: "idle" }));
    }, []);

    const stopAllEngines = useCallback(async () => {
        await Promise.all(
            (["sente", "gote"] as Player[]).map((side) => disposeEngineForSide(side)),
        );
    }, [disposeEngineForSide]);

    const applyMoveFromEngine = useCallback(
        (move: string) => {
            const trimmed = move.trim();
            const result = applyMoveWithState(positionRef.current, trimmed, {
                validateTurn: false,
            });
            if (!result.ok) {
                setErrorLogs((prev) =>
                    [
                        `engine move rejected (${trimmed || "empty"}): ${result.error ?? "unknown"}`,
                        ...prev,
                    ].slice(0, maxLogs),
                );
                return;
            }
            onMoveFromEngine(trimmed);
        },
        [maxLogs, onMoveFromEngine, positionRef],
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

                    setEngineStatus((prev) => ({ ...prev, [side]: "idle" }));
                    searchState.pending = false;
                    searchState.handle = null;

                    const current = activeSearchRef.current;
                    if (current && current.engineId === engineId && current.side === side) {
                        searchState.requestPly = movesRef.current.length;

                        const trimmed = event.move.trim();
                        const token = trimmed.toLowerCase();
                        if (token === "resign" || token === "win" || token === "none") {
                            activeSearchRef.current = null;
                            const sideLabel = side === "sente" ? "先手" : "後手";
                            const opponentLabel = side === "sente" ? "後手" : "先手";
                            if (token === "win") {
                                onMatchEnd(
                                    `対局終了: ${sideLabel}が勝利宣言しました（win）。`,
                                ).catch((err) => {
                                    setErrorLogs((prev) =>
                                        [`対局終了処理でエラー: ${String(err)}`, ...prev].slice(
                                            0,
                                            maxLogs,
                                        ),
                                    );
                                });
                            } else if (token === "resign") {
                                onMatchEnd(
                                    `対局終了: ${sideLabel}が投了しました（resign）。${opponentLabel}の勝ち。`,
                                ).catch((err) => {
                                    setErrorLogs((prev) =>
                                        [`対局終了処理でエラー: ${String(err)}`, ...prev].slice(
                                            0,
                                            maxLogs,
                                        ),
                                    );
                                });
                            } else {
                                onMatchEnd(
                                    `対局終了: ${sideLabel}が合法手なし（bestmove none）。${opponentLabel}の勝ち。`,
                                ).catch((err) => {
                                    setErrorLogs((prev) =>
                                        [`対局終了処理でエラー: ${String(err)}`, ...prev].slice(
                                            0,
                                            maxLogs,
                                        ),
                                    );
                                });
                            }
                            return;
                        }
                        applyMoveFromEngine(trimmed);
                        activeSearchRef.current = null;
                    }
                }
                if (event.type === "error") {
                    const searchState = searchStatesRef.current[side];

                    setEngineStatus((prev) => ({ ...prev, [side]: "error" }));
                    searchState.handle = null;
                    searchState.pending = false;

                    setErrorLogs((prev) => [event.message, ...prev].slice(0, maxLogs));
                }
            });

            engineState.subscription = unsub;
        },
        [applyMoveFromEngine, maxLogs, movesRef, onMatchEnd],
    );

    const ensureEngineReady = useCallback(
        async (side: Player): Promise<{ client: EngineClient; engineId: string } | null> => {
            const setting = sides[side];
            if (setting.role !== "engine") return null;
            const selectedId = setting.engineId ?? engineOptions[0]?.id;
            if (!selectedId) return null;
            const opt = engineMap.get(selectedId);
            if (!opt) return null;

            const engineState = engineStatesRef.current[side];

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
            disposeEngineForSide(side).catch(() => undefined);
        }
    }, [disposeEngineForSide, sides]);

    // アンマウント時に全エンジンを破棄
    useEffect(() => {
        return () => {
            Promise.all(
                (["sente", "gote"] as Player[]).map((side) => disposeEngineForSide(side)),
            ).catch(() => undefined);
        };
    }, [disposeEngineForSide]);

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
            setErrorLogs((prev) => [`engine error: ${String(error)}`, ...prev].slice(0, maxLogs));
        });
    }, [
        getEngineForSide,
        isEngineTurn,
        isMatchRunning,
        maxLogs,
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
    };
}
