import {
    applyMoveWithState,
    type BoardState,
    boardToMatrix,
    cloneBoard,
    createEmptyHands,
    getAllSquares,
    getPositionService,
    type LastMove,
    movesToCsa,
    type Piece,
    type PieceType,
    type Player,
    type PositionState,
    parseCsaMoves,
    parseMove,
    type Square,
} from "@shogi/app-core";
import type { EngineClient, EngineEvent, SearchHandle } from "@shogi/engine-client";
import type { CSSProperties, ReactElement } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
// TODO: usePositionEditor を統合する（Phase 3b）
// import { usePositionEditor } from "./shogi-match/hooks/usePositionEditor";
import { Button } from "./button";
import type { ShogiBoardCell } from "./shogi-board";
import { ShogiBoard } from "./shogi-board";
import { EditModePanel } from "./shogi-match/components/EditModePanel";
import {
    MatchSettingsPanel,
    type SideSetting,
    type EngineOption,
} from "./shogi-match/components/MatchSettingsPanel";
import { type ClockSettings, useClockManager } from "./shogi-match/hooks/useClockManager";
import type { PromotionSelection } from "./shogi-match/types";
import {
    addToHand,
    cloneHandsState,
    consumeFromHand,
    countPieces,
} from "./shogi-match/utils/boardUtils";
import { PIECE_CAP, PIECE_LABELS } from "./shogi-match/utils/constants";
import { parseUsiInput } from "./shogi-match/utils/kifuUtils";
import { LegalMoveCache } from "./shogi-match/utils/legalMoveCache";
import { determinePromotion } from "./shogi-match/utils/promotionLogic";
import { formatTime } from "./shogi-match/utils/timeFormat";
import { TooltipProvider } from "./tooltip";

type Selection = { kind: "square"; square: string } | { kind: "hand"; piece: PieceType };
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

export interface ShogiMatchProps {
    engineOptions: EngineOption[];
    defaultSides?: { sente: SideSetting; gote: SideSetting };
    initialMainTimeMs?: number;
    initialByoyomiMs?: number;
    maxLogs?: number;
    fetchLegalMoves?: (sfen: string, moves: string[]) => Promise<string[]>;
}

// デフォルト値の定数
const DEFAULT_BYOYOMI_MS = 5_000; // デフォルト秒読み時間（5秒）
const DEFAULT_MAX_LOGS = 80; // ログ履歴の最大保持件数
const TOOLTIP_DELAY_DURATION_MS = 120; // ツールチップ表示遅延

const HAND_ORDER: PieceType[] = ["R", "B", "G", "S", "N", "L", "P"];

const baseCard: CSSProperties = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "14px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
};

const clonePositionState = (pos: PositionState): PositionState => ({
    board: cloneBoard(pos.board),
    hands: cloneHandsState(pos.hands),
    turn: pos.turn,
    ply: pos.ply,
});

function formatEvent(event: EngineEvent, engineId: string): string {
    const prefix = `[${engineId}] `;
    if (event.type === "bestmove") {
        return (
            prefix +
            (event.ponder
                ? `bestmove ${event.move} (ponder ${event.ponder})`
                : `bestmove ${event.move}`)
        );
    }
    if (event.type === "info") {
        const score =
            event.scoreMate !== undefined
                ? `mate ${event.scoreMate}`
                : event.scoreCp !== undefined
                  ? `cp ${event.scoreCp}`
                  : "";
        return (
            prefix +
            [
                `info depth ${event.depth ?? "-"}`,
                event.nodes !== undefined ? `nodes ${event.nodes}` : null,
                event.nps !== undefined ? `nps ${event.nps}` : null,
                score ? `score ${score}` : null,
            ]
                .filter(Boolean)
                .join(" ")
        );
    }
    return `${prefix}error ${event.message}`;
}

function boardToGrid(board: BoardState): ShogiBoardCell[][] {
    const matrix = boardToMatrix(board);
    return matrix.map((row) =>
        row.map((cell) => ({
            id: cell.square,
            piece: cell.piece
                ? {
                      owner: cell.piece.owner,
                      type: cell.piece.type,
                      promoted: cell.piece.promoted,
                  }
                : null,
        })),
    );
}

export function ShogiMatch({
    engineOptions,
    defaultSides = {
        sente: { role: "human" },
        gote: { role: "engine", engineId: engineOptions[0]?.id },
    },
    initialMainTimeMs = 0,
    initialByoyomiMs = DEFAULT_BYOYOMI_MS,
    maxLogs = DEFAULT_MAX_LOGS,
    fetchLegalMoves,
}: ShogiMatchProps): ReactElement {
    const hasEngines = engineOptions.length > 0;
    const emptyBoard = useMemo<BoardState>(
        () => Object.fromEntries(getAllSquares().map((sq) => [sq, null])) as BoardState,
        [],
    );
    const [sides, setSides] = useState<{ sente: SideSetting; gote: SideSetting }>(defaultSides);
    const [position, setPosition] = useState<PositionState>({
        board: emptyBoard,
        hands: createEmptyHands(),
        turn: "sente",
        ply: 1,
    });
    const [initialBoard, setInitialBoard] = useState<BoardState | null>(null);
    const [positionReady, setPositionReady] = useState(false);
    const [moves, setMoves] = useState<string[]>([]);
    const [lastMove, setLastMove] = useState<LastMove | undefined>(undefined);
    const [selection, setSelection] = useState<Selection | null>(null);
    const [promotionSelection, setPromotionSelection] = useState<PromotionSelection | null>(null);
    const [engineReady, setEngineReady] = useState<Record<Player, boolean>>({
        sente: false,
        gote: false,
    });
    const [engineStatus, setEngineStatus] = useState<Record<Player, EngineStatus>>({
        sente: "idle",
        gote: "idle",
    });
    const [message, setMessage] = useState<string | null>(null);
    const [flipBoard, setFlipBoard] = useState(false);
    const [timeSettings, setTimeSettings] = useState<ClockSettings>({
        sente: { mainMs: initialMainTimeMs, byoyomiMs: initialByoyomiMs },
        gote: { mainMs: initialMainTimeMs, byoyomiMs: initialByoyomiMs },
    });
    const [eventLogs, setEventLogs] = useState<string[]>([]);
    const [errorLogs, setErrorLogs] = useState<string[]>([]);
    const [isMatchRunning, setIsMatchRunning] = useState(false);
    const [isEditMode, setIsEditMode] = useState(true);
    const [editOwner, setEditOwner] = useState<Player>("sente");
    const [editPieceType, setEditPieceType] = useState<PieceType | null>(null);
    const [editPromoted, setEditPromoted] = useState(false);
    const [editFromSquare, setEditFromSquare] = useState<Square | null>(null);
    const [editTool, setEditTool] = useState<"place" | "erase">("place");
    const [startSfen, setStartSfen] = useState<string>("startpos");
    const [basePosition, setBasePosition] = useState<PositionState | null>(null);
    const [isEditPanelOpen, setIsEditPanelOpen] = useState(false);
    const [isSettingsPanelOpen, setIsSettingsPanelOpen] = useState(false);

    // エンジン状態管理用の統合されたRef
    const searchStatesRef = useRef<Record<Player, SearchState>>({
        sente: { handle: null, pending: false, requestPly: null },
        gote: { handle: null, pending: false, requestPly: null },
    });
    const engineStatesRef = useRef<Record<Player, EngineState>>({
        sente: { client: null, subscription: null, selectedId: null, ready: false },
        gote: { client: null, subscription: null, selectedId: null, ready: false },
    });
    const activeSearchRef = useRef<ActiveSearch | null>(null);

    const positionRef = useRef<PositionState>(position);
    const movesRef = useRef<string[]>(moves);
    const legalCache = useMemo(() => new LegalMoveCache(), []);
    const matchEndedRef = useRef(false);
    const boardSectionRef = useRef<HTMLDivElement>(null);
    const settingsLocked = isMatchRunning;

    // 時計管理フックを使用
    const { clocks, resetClocks, updateClocksForNextTurn, stopTicking, startTicking } =
        useClockManager({
            timeSettings,
            isMatchRunning,
            onTimeExpired: async (side) => {
                const loserLabel = side === "sente" ? "先手" : "後手";
                const winnerLabel = side === "sente" ? "後手" : "先手";
                await endMatch(`対局終了: ${loserLabel}が時間切れ。${winnerLabel}の勝ち。`);
            },
            matchEndedRef,
        });

    useEffect(() => {
        let cancelled = false;
        const service = getPositionService();

        const init = async () => {
            try {
                const pos = await service.getInitialBoard();
                if (cancelled) return;
                setPosition(pos);
                positionRef.current = pos;
                setInitialBoard(cloneBoard(pos.board));
                setBasePosition(clonePositionState(pos));
                try {
                    const sfen = await service.boardToSfen(pos);
                    if (!cancelled) {
                        setStartSfen(sfen);
                    }
                } catch (error) {
                    if (!cancelled) {
                        setMessage(`局面のSFEN変換に失敗しました: ${String(error)}`);
                    }
                }
                if (!cancelled) {
                    setPositionReady(true);
                }
            } catch (error) {
                if (!cancelled) {
                    setMessage(`初期局面の取得に失敗しました: ${String(error)}`);
                }
            }
        };

        void init();
        return () => {
            cancelled = true;
        };
    }, []);

    const engineMap = useMemo(() => {
        const map = new Map<string, EngineOption>();
        for (const opt of engineOptions) {
            map.set(opt.id, opt);
        }
        return map;
    }, [engineOptions]);

    const disposeEngineForSide = useCallback(async (side: Player) => {
        const engineState = engineStatesRef.current[side];
        const searchState = searchStatesRef.current[side];

        // サブスクリプションのクリーンアップ
        if (engineState.subscription) {
            engineState.subscription();
            engineState.subscription = null;
        }

        // 検索ハンドルのキャンセル
        if (searchState.handle) {
            await searchState.handle.cancel().catch(() => undefined);
            searchState.handle = null;
        }

        // 検索状態のクリア
        searchState.pending = false;
        searchState.requestPly = null;

        // アクティブ検索のクリア
        if (activeSearchRef.current?.side === side) {
            activeSearchRef.current = null;
        }

        // エンジンクライアントの停止と破棄
        if (engineState.client) {
            await engineState.client.stop().catch(() => undefined);
            if (typeof engineState.client.dispose === "function") {
                await engineState.client.dispose().catch(() => undefined);
            }
        }

        // エンジン状態のクリア
        engineState.client = null;
        engineState.selectedId = null;
        engineState.ready = false;

        setEngineReady((prev) => ({ ...prev, [side]: false }));
        setEngineStatus((prev) => ({ ...prev, [side]: "idle" }));
    }, []);

    const grid = useMemo(() => {
        const g = boardToGrid(position.board);
        return flipBoard ? [...g].reverse().map((row) => [...row].reverse()) : g;
    }, [position.board, flipBoard]);

    const getEngineForSide = useCallback(
        (side: Player): EngineOption | undefined => {
            if (!hasEngines) return undefined;
            const setting = sides[side];
            if (setting.role !== "engine") return undefined;
            const fallback = engineOptions[0];
            if (setting.engineId && engineMap.has(setting.engineId)) {
                return engineMap.get(setting.engineId);
            }
            return fallback;
        },
        [engineMap, engineOptions, hasEngines, sides],
    );

    const isEngineTurn = useCallback(
        (side: Player): boolean => {
            return sides[side].role === "engine" && Boolean(getEngineForSide(side));
        },
        [getEngineForSide, sides],
    );

    const refreshStartSfen = useCallback(async (pos: PositionState) => {
        try {
            const sfen = await getPositionService().boardToSfen(pos);
            setStartSfen(sfen);
        } catch (error) {
            setMessage(`局面のSFEN変換に失敗しました: ${String(error)}`);
            throw error;
        }
    }, []);

    const stopAllEngines = useCallback(async () => {
        for (const side of ["sente", "gote"] as Player[]) {
            const searchState = searchStatesRef.current[side];

            // 検索ハンドルのキャンセル
            if (searchState.handle) {
                await searchState.handle.cancel().catch(() => undefined);
                searchState.handle = null;
            }

            // 検索状態のクリア
            searchState.pending = false;
            searchState.requestPly = null;
        }

        // アクティブ検索のクリア
        activeSearchRef.current = null;

        setEngineStatus({ sente: "idle", gote: "idle" });
    }, []);

    const endMatch = useCallback(
        async (nextMessage: string) => {
            if (matchEndedRef.current) return;
            matchEndedRef.current = true;
            setMessage(nextMessage);
            setIsMatchRunning(false);
            stopTicking();
            await stopAllEngines();
        },
        [stopAllEngines, stopTicking],
    );

    const pauseAutoPlay = async () => {
        setIsMatchRunning(false);
        stopTicking();
        await stopAllEngines();
    };

    const resumeAutoPlay = async () => {
        matchEndedRef.current = false;
        if (!positionReady) return;
        if (isEditMode) {
            await finalizeEditedPosition();
            // 対局開始時に編集モードを終了し、パネルを閉じる
            setIsEditMode(false);
            setIsEditPanelOpen(false);
        }
        // 対局開始時に設定パネルを閉じる
        setIsSettingsPanelOpen(false);
        // 盤面セクションにスクロール
        setTimeout(() => {
            boardSectionRef.current?.scrollIntoView({
                behavior: "smooth",
                block: "start",
            });
        }, 100);
        const turn = position.turn;

        if (isEngineTurn(turn)) {
            try {
                setMessage("エンジン初期化中…（初回は数秒かかる場合があります）");
                const engineSides = (["sente", "gote"] as Player[]).filter((side) =>
                    isEngineTurn(side),
                );
                if (engineSides.length >= 2) {
                    await Promise.all(engineSides.map((side) => ensureEngineReady(side)));
                } else {
                    await ensureEngineReady(turn);
                }
                setMessage(null);
            } catch (error) {
                setEngineStatus((prev) => ({ ...prev, [turn]: "error" }));
                setErrorLogs((prev) =>
                    [`engine error: ${String(error)}`, ...prev].slice(0, maxLogs),
                );
                setMessage(`エンジン初期化に失敗しました: ${String(error)}`);
                return;
            }
        } else {
            for (const side of ["sente", "gote"] as Player[]) {
                if (!isEngineTurn(side)) continue;
                ensureEngineReady(side).catch(() => undefined);
            }
        }

        setIsMatchRunning(true);
        startTicking(turn);
        if (!isEngineTurn(turn)) return;
        try {
            await startEngineTurn(turn);
        } catch (error) {
            setEngineStatus((prev) => ({ ...prev, [turn]: "error" }));
            setErrorLogs((prev) => [`engine error: ${String(error)}`, ...prev].slice(0, maxLogs));
        }
    };

    const finalizeEditedPosition = async () => {
        if (isMatchRunning) return;
        const current = positionRef.current;
        setBasePosition(clonePositionState(current));
        setInitialBoard(cloneBoard(current.board));
        await refreshStartSfen(current);
        legalCache.clear();
        setIsEditMode(false);
        setMessage("局面を確定しました。対局開始でこの局面から進行します。");
    };

    const resetToBasePosition = useCallback(async () => {
        matchEndedRef.current = false;
        await stopAllEngines();
        const service = getPositionService();
        let next = basePosition ? clonePositionState(basePosition) : null;
        if (!next) {
            try {
                const fetched = await service.getInitialBoard();
                next = clonePositionState(fetched);
                setBasePosition(clonePositionState(fetched));
                try {
                    const sfen = await service.boardToSfen(fetched);
                    setStartSfen(sfen);
                } catch {
                    setStartSfen("startpos");
                }
            } catch (error) {
                setMessage(`初期局面の再取得に失敗しました: ${String(error)}`);
                return;
            }
        }
        setPosition(next);
        positionRef.current = next;
        setInitialBoard(cloneBoard(next.board));
        setPositionReady(true);
        setMoves([]);
        setLastMove(undefined);
        setSelection(null);
        setMessage(null);
        resetClocks(false);

        // 検索状態のリセット
        searchStatesRef.current.sente.requestPly = null;
        searchStatesRef.current.gote.requestPly = null;
        searchStatesRef.current.sente.pending = false;
        searchStatesRef.current.gote.pending = false;

        setIsMatchRunning(false);
        setIsEditMode(true);
        setEditFromSquare(null);
        setEditTool("place");
        setEditPromoted(false);
        setEditOwner("sente");
        setEditPieceType(null);

        activeSearchRef.current = null;

        setErrorLogs([]);
        setEventLogs([]);
        legalCache.clear();
        void refreshStartSfen(next);
    }, [basePosition, refreshStartSfen, resetClocks, stopAllEngines, legalCache.clear]);

    const applyMoveCommon = useCallback(
        (nextPosition: PositionState, mv: string, last?: LastMove) => {
            setPosition(nextPosition);
            setMoves((prev) => [...prev, mv]);
            setLastMove(last);
            setSelection(null);
            setMessage(null);
            activeSearchRef.current = null;
            legalCache.clear();
            updateClocksForNextTurn(nextPosition.turn);
        },
        [updateClocksForNextTurn, legalCache.clear],
    );

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
            applyMoveCommon(result.next, trimmed, result.lastMove);
        },
        [applyMoveCommon, maxLogs],
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
                                endMatch(`対局終了: ${sideLabel}が勝利宣言しました（win）。`).catch(
                                    (err) => {
                                        setErrorLogs((prev) =>
                                            [`対局終了処理でエラー: ${String(err)}`, ...prev].slice(
                                                0,
                                                maxLogs,
                                            ),
                                        );
                                    },
                                );
                            } else if (token === "resign") {
                                endMatch(
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
                                endMatch(
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
        [applyMoveFromEngine, endMatch, maxLogs],
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
        [attachSubscription, disposeEngineForSide, engineMap, engineOptions, sides, startSfen],
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
        [ensureEngineReady, positionReady, startSfen, timeSettings],
    );

    useEffect(() => {
        positionRef.current = position;
    }, [position]);

    useEffect(() => {
        movesRef.current = moves;
    }, [moves]);

    useEffect(() => {
        for (const side of ["sente", "gote"] as Player[]) {
            if (sides[side].role === "engine") continue;
            disposeEngineForSide(side).catch(() => undefined);
        }
    }, [disposeEngineForSide, sides]);

    useEffect(() => {
        return () => {
            // アンマウント時のみ全エンジンを停止・解放する。
            Promise.all(
                (["sente", "gote"] as Player[]).map((side) => disposeEngineForSide(side)),
            ).catch(() => undefined);
        };
        // disposeEngineForSide は安定化済みなので、依存配列に含めても再実行されない。
    }, [disposeEngineForSide]);

    useEffect(() => {
        if (!isMatchRunning || !positionReady) return;
        const side = position.turn;
        if (!isEngineTurn(side)) return;
        const engineOpt = getEngineForSide(side);
        if (!engineOpt) return;

        const searchState = searchStatesRef.current[side];
        const current = activeSearchRef.current;

        if (current && current.side === side && current.engineId === engineOpt.id) {
            return;
        }
        if (searchState.requestPly === moves.length) return;

        searchState.requestPly = moves.length;

        startEngineTurn(side).catch((error) => {
            setEngineStatus((prev) => ({ ...prev, [side]: "error" }));
            setErrorLogs((prev) => [`engine error: ${String(error)}`, ...prev].slice(0, maxLogs));
        });
    }, [
        getEngineForSide,
        isEngineTurn,
        isMatchRunning,
        maxLogs,
        moves.length,
        position.turn,
        positionReady,
        startEngineTurn,
    ]);

    const handleNewGame = async () => {
        await resetToBasePosition();
    };

    const getLegalSet = async (): Promise<Set<string> | null> => {
        if (!positionReady) return null;
        const ply = movesRef.current.length;
        const resolver = async () => {
            if (fetchLegalMoves) {
                return fetchLegalMoves(startSfen, movesRef.current);
            }
            return getPositionService().getLegalMoves(startSfen, movesRef.current);
        };
        return legalCache.getOrResolve(ply, resolver);
    };

    const applyEditedPosition = (nextPosition: PositionState) => {
        setPosition(nextPosition);
        positionRef.current = nextPosition;
        setInitialBoard(cloneBoard(nextPosition.board));
        setMoves([]);
        movesRef.current = [];
        setLastMove(undefined);
        setSelection(null);
        setMessage(null);
        setEditFromSquare(null);

        // 検索状態のリセット
        searchStatesRef.current.sente.requestPly = null;
        searchStatesRef.current.gote.requestPly = null;

        activeSearchRef.current = null;

        legalCache.clear();
        stopTicking();
        matchEndedRef.current = false;
        setIsMatchRunning(false);
        void refreshStartSfen(nextPosition);
    };

    const clearBoardForEdit = () => {
        if (isMatchRunning) return;
        const emptyBoard = Object.fromEntries(
            getAllSquares().map((sq) => [sq, null]),
        ) as BoardState;
        const next: PositionState = {
            board: emptyBoard,
            hands: createEmptyHands(),
            turn: "sente",
            ply: 1,
        };
        applyEditedPosition(next);
        setMessage("盤面をクリアしました。");
    };

    const resetToStartposForEdit = async () => {
        if (isMatchRunning) return;
        try {
            const service = getPositionService();
            const pos = await service.getInitialBoard();
            applyEditedPosition(clonePositionState(pos));
            setInitialBoard(cloneBoard(pos.board));
            setMessage("平手初期化しました。");
        } catch (error) {
            setMessage(`平手初期化に失敗しました: ${String(error)}`);
        }
    };

    const updateTurnForEdit = (turn: Player) => {
        if (isMatchRunning) return;
        const current = positionRef.current;
        applyEditedPosition({ ...current, turn });
    };

    const placePieceAt = (
        square: Square,
        piece: Piece | null,
        options?: { fromSquare?: Square },
    ): boolean => {
        const current = positionRef.current;
        const nextBoard = cloneBoard(current.board);
        let workingHands = cloneHandsState(current.hands);

        if (options?.fromSquare) {
            nextBoard[options.fromSquare] = null;
        }

        const existing = nextBoard[square];
        if (existing) {
            const base = existing.type;
            workingHands = addToHand(workingHands, existing.owner, base);
        }

        if (!piece) {
            nextBoard[square] = null;
            const nextPosition: PositionState = {
                ...current,
                board: nextBoard,
                hands: workingHands,
            };
            applyEditedPosition(nextPosition);
            return true;
        }

        const baseType = piece.type;
        const consumedHands = consumeFromHand(workingHands, piece.owner, baseType);
        const handsForPlacement = consumedHands ?? workingHands;
        const countsBefore = countPieces({
            ...current,
            board: nextBoard,
            hands: handsForPlacement,
        });
        const nextCount = countsBefore[piece.owner][baseType] + 1;
        if (nextCount > PIECE_CAP[baseType]) {
            setMessage(
                `${piece.owner === "sente" ? "先手" : "後手"}の${PIECE_LABELS[baseType]}は最大${PIECE_CAP[baseType]}枚までです`,
            );
            return false;
        }
        if (piece.type === "K" && countsBefore[piece.owner][baseType] >= PIECE_CAP.K) {
            setMessage("玉はそれぞれ1枚まで配置できます。");
            return false;
        }

        nextBoard[square] = piece.promoted ? { ...piece, promoted: true } : { ...piece };
        const finalHands = consumedHands ?? workingHands;
        const nextPosition: PositionState = {
            ...current,
            board: nextBoard,
            hands: finalHands,
        };
        applyEditedPosition(nextPosition);
        return true;
    };

    const handleSquareSelect = async (square: string, shiftKey?: boolean) => {
        setMessage(null);
        if (isEditMode) {
            if (!positionReady) {
                setMessage("局面を読み込み中です。");
                return;
            }
            // 編集パネルが閉じていたら自動的に開く
            if (!isEditPanelOpen) {
                setIsEditPanelOpen(true);
            }
            const sq = square as Square;

            // 移動元が選択されている場合：移動先として処理
            if (editFromSquare) {
                const from = editFromSquare;
                if (from === sq) {
                    // 同じマスをクリック：選択解除
                    setEditFromSquare(null);
                    return;
                }
                const moving = position.board[from];
                if (!moving) {
                    setEditFromSquare(null);
                    return;
                }
                const ok = placePieceAt(sq, moving, { fromSquare: from });
                if (ok) {
                    setEditFromSquare(null);
                }
                return;
            }

            // 削除モード：駒を削除
            if (editTool === "erase") {
                placePieceAt(sq, null);
                return;
            }

            // 駒ボタンが選択されている場合：配置
            if (editPieceType) {
                const pieceToPlace: Piece = {
                    owner: editOwner,
                    type: editPieceType,
                    promoted: editPromoted || undefined,
                };
                placePieceAt(sq, pieceToPlace);
                return;
            }

            // 駒ボタン未選択：盤上の駒をクリックで移動元として選択
            const current = position.board[sq];
            if (current) {
                setEditFromSquare(sq);
                return;
            }

            // 空マスをクリックした場合
            setMessage("配置する駒を選ぶか、移動する駒をクリックしてください。");
            return;
        }
        if (!positionReady) {
            setMessage("局面を読み込み中です。");
            return;
        }
        if (isEngineTurn(position.turn)) {
            setMessage("エンジンの手番です。");
            return;
        }

        // 成り選択中の場合：成り/不成を選択
        if (promotionSelection) {
            // 成り選択UIの外をクリック → キャンセル
            setPromotionSelection(null);
            setSelection(null);
            return;
        }

        if (!selection) {
            const sq = square as Square;
            const piece = position.board[sq];
            if (piece && piece.owner === position.turn) {
                setSelection({ kind: "square", square: sq });
            }
            return;
        }

        if (selection.kind === "square") {
            if (selection.square === square) {
                setSelection(null);
                return;
            }

            const legal = await getLegalSet();
            if (!legal) return;

            const from = selection.square;
            const to = square;
            const piece = position.board[from as Square];

            // 成り判定を実行
            const promotion = determinePromotion(legal, from, to);

            // 【ケース1】成れない場合 → 基本移動を試行
            if (promotion === "none") {
                const moveStr = `${from}${to}`;
                if (!legal.has(moveStr)) {
                    setMessage("合法手ではありません");
                    return;
                }
                const result = applyMoveWithState(position, moveStr, { validateTurn: true });
                if (!result.ok) {
                    setMessage(result.error ?? "指し手を適用できませんでした");
                    return;
                }
                applyMoveCommon(result.next, moveStr, result.lastMove);
                return;
            }

            // 【ケース2】強制成り → 自動的に成って移動（ダイアログなし）
            if (promotion === "forced") {
                const moveStr = `${from}${to}+`;
                const result = applyMoveWithState(position, moveStr, { validateTurn: true });
                if (!result.ok) {
                    setMessage(result.error ?? "指し手を適用できませんでした");
                    return;
                }
                applyMoveCommon(result.next, moveStr, result.lastMove);
                return;
            }

            // 【ケース3】任意成り（promotion === 'optional'）
            // Shift+クリック：即座に成って移動
            if (shiftKey) {
                const moveStr = `${from}${to}+`;
                const result = applyMoveWithState(position, moveStr, { validateTurn: true });
                if (!result.ok) {
                    setMessage(result.error ?? "指し手を適用できませんでした");
                    return;
                }
                applyMoveCommon(result.next, moveStr, result.lastMove);
                return;
            }

            // 通常クリック：成り選択ダイアログを表示
            if (!piece) {
                setMessage("駒が見つかりません");
                return;
            }
            setPromotionSelection({ from: from as Square, to: to as Square, piece });
            return;
        }

        // 持ち駒を打つ
        const moveStr = `${selection.piece}*${square}`;
        const legal = await getLegalSet();
        if (legal && !legal.has(moveStr)) {
            setMessage("合法手ではありません");
            return;
        }
        const result = applyMoveWithState(position, moveStr, { validateTurn: true });
        if (!result.ok) {
            setMessage(result.error ?? "持ち駒を打てませんでした");
            return;
        }
        applyMoveCommon(result.next, moveStr, result.lastMove);
    };

    const handlePromotionChoice = (promote: boolean) => {
        if (!promotionSelection) return;
        const { from, to } = promotionSelection;
        const moveStr = `${from}${to}${promote ? "+" : ""}`;
        const result = applyMoveWithState(position, moveStr, { validateTurn: true });
        if (!result.ok) {
            setMessage(result.error ?? "指し手を適用できませんでした");
            setPromotionSelection(null);
            setSelection(null);
            return;
        }
        applyMoveCommon(result.next, moveStr, result.lastMove);
        setPromotionSelection(null);
    };

    const handleHandSelect = (piece: PieceType) => {
        if (!positionReady) {
            setMessage("局面を読み込み中です。");
            return;
        }
        if (isEditMode) {
            setMessage("編集モード中は手番入力は無効です。盤面編集パネルを使ってください。");
            return;
        }
        if (isEngineTurn(position.turn)) {
            setMessage("エンジンの手番です。");
            return;
        }
        setSelection({ kind: "hand", piece });
        setMessage(null);
    };

    const deriveLastMove = (move: string | undefined): LastMove | undefined => {
        const parsed = move ? parseMove(move) : null;
        if (!parsed) return undefined;
        if (parsed.kind === "drop") {
            return { from: null, to: parsed.to, dropPiece: parsed.piece, promotes: false };
        }
        return { from: parsed.from, to: parsed.to, promotes: parsed.promote };
    };

    const importUsi = async (raw: string) => {
        const tokens = parseUsiInput(raw);
        if (tokens.length === 0) return;
        await loadMoves(tokens);
    };

    const loadMoves = async (list: string[]) => {
        const filtered = list.filter(Boolean);
        const service = getPositionService();
        try {
            const result = await service.replayMovesStrict(startSfen, filtered);
            setPosition(result.position);
            positionRef.current = result.position;
            setMoves(result.applied);
            setLastMove(deriveLastMove(result.applied.at(-1)));
            setSelection(null);
            setMessage(result.error ?? null);
            resetClocks(false);

            // 検索状態のリセット
            searchStatesRef.current.sente.requestPly = null;
            searchStatesRef.current.gote.requestPly = null;

            legalCache.clear();
            setPositionReady(true);
        } catch (error) {
            setMessage(`棋譜の適用に失敗しました: ${String(error)}`);
        }
    };

    const exportUsi = moves.length ? `${startSfen} moves ${moves.join(" ")}` : startSfen;
    const exportCsa = useMemo(
        () => (positionReady && initialBoard ? movesToCsa(moves, {}, initialBoard) : ""),
        [initialBoard, moves, positionReady],
    );

    const handView = (owner: Player) => {
        const hand = position.hands[owner];
        const ownerSetting = sides[owner];
        const isActive = !isEditMode && position.turn === owner && ownerSetting.role === "human";
        return (
            <div style={{ display: "flex", flexWrap: "wrap", gap: "6px" }}>
                {HAND_ORDER.map((piece) => {
                    const count = hand[piece] ?? 0;
                    const selected = selection?.kind === "hand" && selection.piece === piece;
                    return (
                        <button
                            key={`${owner}-${piece}`}
                            type="button"
                            onClick={() => handleHandSelect(piece)}
                            disabled={count <= 0 || !isActive}
                            style={{
                                minWidth: "52px",
                                padding: "6px 10px",
                                borderRadius: "10px",
                                border: selected
                                    ? "2px solid hsl(var(--primary, 15 86% 55%))"
                                    : "1px solid hsl(var(--border, 0 0% 86%))",
                                background:
                                    count > 0
                                        ? "hsl(var(--secondary, 210 40% 96%))"
                                        : "transparent",
                                color: "hsl(var(--foreground, 222 47% 11%))",
                                cursor: count > 0 && isActive ? "pointer" : "not-allowed",
                            }}
                        >
                            {PIECE_LABELS[piece]} × {count}
                        </button>
                    );
                })}
            </div>
        );
    };

    const renderClock = (side: Player) => {
        const clock = clocks[side];
        const ticking = clocks.ticking === side;
        return (
            <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
                <span
                    style={{
                        fontWeight: 700,
                        color:
                            side === "sente"
                                ? "hsl(var(--primary, 15 86% 55%))"
                                : "hsl(var(--accent, 37 94% 50%))",
                    }}
                >
                    {side === "sente" ? "先手" : "後手"}
                </span>
                <span style={{ fontVariantNumeric: "tabular-nums", fontSize: "16px" }}>
                    {formatTime(clock.mainMs)} + {formatTime(clock.byoyomiMs)}
                </span>
                {ticking ? (
                    <span
                        style={{
                            display: "inline-block",
                            width: "10px",
                            height: "10px",
                            borderRadius: "50%",
                            background: "hsl(var(--primary, 15 86% 55%))",
                        }}
                    />
                ) : null}
            </div>
        );
    };

    const candidateNote = positionReady ? null : "局面を読み込み中です。";

    const uiEngineOptions = useMemo(() => {
        // 内蔵エンジンの A/B スロットは UI に露出させず、単一の「内蔵エンジン」として扱う。
        const internal = engineOptions.find((opt) => opt.kind === "internal") ?? engineOptions[0];
        const externals = engineOptions.filter((opt) => opt.kind === "external");
        const list: EngineOption[] = [];
        if (internal) list.push({ ...internal, id: internal.id, label: "内蔵エンジン" });
        return [...list, ...externals];
    }, [engineOptions]);

    return (
        <TooltipProvider delayDuration={TOOLTIP_DELAY_DURATION_MS}>
            <section
                style={{
                    ...baseCard,
                    padding: "16px",
                    display: "flex",
                    flexDirection: "column",
                    gap: "12px",
                }}
            >
                <div
                    style={{
                        display: "flex",
                        justifyContent: "space-between",
                        alignItems: "center",
                        gap: "12px",
                    }}
                >
                    <div>
                        <div style={{ fontWeight: 700 }}>盤 UI + 対局</div>
                        <div
                            style={{
                                color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                fontSize: "13px",
                            }}
                        >
                            先手・後手それぞれに「人間 /
                            エンジン」を割り当てて試せます。クリック2回で移動、持ち駒はボタン→マスで打ち込み。
                        </div>
                    </div>
                    <div style={{ display: "flex", gap: "8px", alignItems: "center" }}>
                        <label
                            style={{
                                display: "flex",
                                alignItems: "center",
                                gap: "6px",
                                fontSize: "13px",
                            }}
                        >
                            <input
                                type="checkbox"
                                checked={flipBoard}
                                onChange={(e) => setFlipBoard(e.target.checked)}
                            />
                            盤面を反転
                        </label>
                    </div>
                </div>

                <div
                    style={{
                        display: "grid",
                        gridTemplateColumns: "minmax(320px, 1fr) 360px",
                        gap: "12px",
                        alignItems: "flex-start",
                    }}
                >
                    <div style={{ display: "flex", flexDirection: "column", gap: "12px" }}>
                        <div ref={boardSectionRef} style={{ ...baseCard, padding: "12px" }}>
                            <div style={{ fontWeight: 700, marginBottom: "8px" }}>盤面</div>
                            <div
                                style={{
                                    marginTop: "8px",
                                    display: "flex",
                                    gap: "8px",
                                    flexDirection: "column",
                                }}
                            >
                                <ShogiBoard
                                    grid={grid}
                                    selectedSquare={
                                        isEditMode && editFromSquare
                                            ? editFromSquare
                                            : selection?.kind === "square"
                                              ? selection.square
                                              : null
                                    }
                                    lastMove={
                                        lastMove
                                            ? { from: lastMove.from ?? undefined, to: lastMove.to }
                                            : undefined
                                    }
                                    promotionSquare={promotionSelection?.to ?? null}
                                    onSelect={(sq, shiftKey) => {
                                        void handleSquareSelect(sq, shiftKey);
                                    }}
                                    onPromotionChoice={handlePromotionChoice}
                                />
                                {candidateNote ? (
                                    <div
                                        style={{
                                            fontSize: "12px",
                                            color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                        }}
                                    >
                                        {candidateNote}
                                    </div>
                                ) : null}
                            </div>
                        </div>

                        <div style={{ ...baseCard, padding: "12px" }}>
                            <div
                                style={{
                                    display: "flex",
                                    justifyContent: "space-between",
                                    alignItems: "center",
                                }}
                            >
                                <div style={{ fontWeight: 700 }}>先手の持ち駒</div>
                                <div
                                    style={{
                                        fontSize: "12px",
                                        color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                    }}
                                >
                                    手番: {position.turn === "sente" ? "先手" : "後手"}
                                </div>
                            </div>
                            <div style={{ marginTop: "8px" }}>{handView("sente")}</div>
                            <div style={{ marginTop: "14px", fontWeight: 700 }}>後手の持ち駒</div>
                            <div style={{ marginTop: "8px" }}>{handView("gote")}</div>
                        </div>
                    </div>

                    <div style={{ display: "flex", flexDirection: "column", gap: "10px" }}>
                        <EditModePanel
                            isOpen={isEditPanelOpen}
                            onOpenChange={setIsEditPanelOpen}
                            editOwner={editOwner}
                            editPieceType={editPieceType}
                            editPromoted={editPromoted}
                            editFromSquare={editFromSquare}
                            editTool={editTool}
                            setEditOwner={setEditOwner}
                            setEditPieceType={setEditPieceType}
                            setEditPromoted={setEditPromoted}
                            setEditTool={setEditTool}
                            onResetToStartpos={resetToStartposForEdit}
                            onClearBoard={clearBoardForEdit}
                            isMatchRunning={isMatchRunning}
                            positionReady={positionReady}
                        />

                        <div
                            style={{
                                ...baseCard,
                                padding: "12px",
                                display: "flex",
                                flexDirection: "column",
                                gap: "10px",
                            }}
                        >
                            <div
                                style={{
                                    display: "flex",
                                    gap: "8px",
                                    flexWrap: "wrap",
                                    alignItems: "center",
                                }}
                            >
                                <Button
                                    type="button"
                                    onClick={handleNewGame}
                                    style={{ paddingInline: "12px" }}
                                >
                                    新規対局（初期化）
                                </Button>
                                <Button
                                    type="button"
                                    onClick={pauseAutoPlay}
                                    variant="outline"
                                    style={{ paddingInline: "12px" }}
                                >
                                    停止（自動進行オフ）
                                </Button>
                                <Button
                                    type="button"
                                    onClick={resumeAutoPlay}
                                    variant="secondary"
                                    style={{ paddingInline: "12px" }}
                                >
                                    対局開始 / 再開
                                </Button>
                            </div>
                            <div
                                style={{
                                    fontSize: "12px",
                                    color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                }}
                            >
                                状態:
                                {(["sente", "gote"] as Player[]).map((side) => {
                                    const roleLabel =
                                        sides[side].role === "engine" ? "エンジン" : "人間";
                                    if (sides[side].role !== "engine") {
                                        return ` [${side === "sente" ? "先手" : "後手"}: ${roleLabel}]`;
                                    }
                                    const engineLabel = getEngineForSide(side)?.label ?? "未選択";
                                    const ready = engineReady[side] ? "init済" : "未init";
                                    const status = engineStatus[side];
                                    return ` [${side === "sente" ? "先手" : "後手"}: ${roleLabel} ${engineLabel} ${status}/${ready}]`;
                                })}
                                {` | 対局: ${isMatchRunning ? "実行中" : "停止中"}`}
                            </div>
                            {message ? (
                                <div
                                    style={{
                                        color: "hsl(var(--destructive, 0 72% 51%))",
                                        fontSize: "13px",
                                    }}
                                >
                                    {message}
                                </div>
                            ) : null}

                            <MatchSettingsPanel
                                isOpen={isSettingsPanelOpen}
                                onOpenChange={setIsSettingsPanelOpen}
                                sides={sides}
                                onSidesChange={setSides}
                                timeSettings={timeSettings}
                                onTimeSettingsChange={setTimeSettings}
                                currentTurn={position.turn}
                                onTurnChange={updateTurnForEdit}
                                uiEngineOptions={uiEngineOptions}
                                settingsLocked={settingsLocked}
                            />
                        </div>

                        <div style={{ ...baseCard, padding: "12px" }}>
                            <div style={{ fontWeight: 700, marginBottom: "6px" }}>時計</div>
                            <div style={{ display: "flex", flexDirection: "column", gap: "6px" }}>
                                {renderClock("sente")}
                                {renderClock("gote")}
                            </div>
                        </div>

                        <div style={{ ...baseCard, padding: "12px" }}>
                            <div style={{ fontWeight: 700, marginBottom: "6px" }}>
                                棋譜 / 入出力
                            </div>
                            <div
                                style={{
                                    fontSize: "13px",
                                    color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                }}
                            >
                                先手から {moves.length} 手目
                            </div>
                            <ol
                                style={{
                                    paddingLeft: "18px",
                                    maxHeight: "160px",
                                    overflow: "auto",
                                    margin: "8px 0",
                                }}
                            >
                                {moves.map((mv, idx) => (
                                    <li
                                        key={`${idx}-${mv}`}
                                        style={{
                                            fontFamily: "ui-monospace, monospace",
                                            fontSize: "13px",
                                        }}
                                    >
                                        {idx + 1}. {mv}
                                    </li>
                                ))}
                            </ol>
                            <div
                                style={{ display: "grid", gridTemplateColumns: "1fr", gap: "8px" }}
                            >
                                <label
                                    style={{ display: "flex", flexDirection: "column", gap: "4px" }}
                                >
                                    USI / SFEN（現在の開始局面 + moves）
                                    <textarea
                                        value={exportUsi}
                                        onChange={(e) => {
                                            void importUsi(e.target.value);
                                        }}
                                        rows={3}
                                        style={{
                                            width: "100%",
                                            padding: "8px",
                                            borderRadius: "8px",
                                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        }}
                                    />
                                </label>
                                <label
                                    style={{ display: "flex", flexDirection: "column", gap: "4px" }}
                                >
                                    CSA
                                    <textarea
                                        value={exportCsa}
                                        onChange={(e) => {
                                            if (!positionReady) return;
                                            void loadMoves(
                                                parseCsaMoves(
                                                    e.target.value,
                                                    initialBoard ?? undefined,
                                                ),
                                            );
                                        }}
                                        rows={3}
                                        style={{
                                            width: "100%",
                                            padding: "8px",
                                            borderRadius: "8px",
                                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        }}
                                    />
                                </label>
                            </div>
                        </div>

                        <div style={{ ...baseCard, padding: "12px" }}>
                            <div style={{ fontWeight: 700, marginBottom: "6px" }}>エンジンログ</div>
                            <ul
                                style={{
                                    listStyle: "none",
                                    padding: 0,
                                    margin: 0,
                                    display: "flex",
                                    flexDirection: "column",
                                    gap: "4px",
                                    maxHeight: "160px",
                                    overflow: "auto",
                                }}
                            >
                                {eventLogs.map((log, idx) => (
                                    <li
                                        key={`${idx}-${log}`}
                                        style={{
                                            fontFamily: "ui-monospace, monospace",
                                            fontSize: "12px",
                                        }}
                                    >
                                        {log}
                                    </li>
                                ))}
                            </ul>
                            {errorLogs.length ? (
                                <div
                                    style={{
                                        marginTop: "8px",
                                        color: "hsl(var(--destructive, 0 72% 51%))",
                                        fontSize: "12px",
                                    }}
                                >
                                    {errorLogs[0]}
                                </div>
                            ) : null}
                        </div>
                    </div>
                </div>
            </section>
        </TooltipProvider>
    );
}
