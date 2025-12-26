import {
    applyMoveWithState,
    type BoardState,
    boardToMatrix,
    cloneBoard,
    createEmptyHands,
    type GameResult,
    getAllSquares,
    getPositionService,
    type LastMove,
    type Piece,
    type PieceType,
    type Player,
    type PositionState,
    parseMove,
    type Square,
} from "@shogi/app-core";
import type { CSSProperties, ReactElement } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ShogiBoardCell } from "./shogi-board";
import { ShogiBoard } from "./shogi-board";
import { ClockDisplayPanel } from "./shogi-match/components/ClockDisplayPanel";
import { EditModePanel } from "./shogi-match/components/EditModePanel";
import { EngineLogsPanel } from "./shogi-match/components/EngineLogsPanel";
import { EvalPanel } from "./shogi-match/components/EvalPanel";
import { GameResultBanner } from "./shogi-match/components/GameResultBanner";
import { GameResultDialog } from "./shogi-match/components/GameResultDialog";
import { HandPiecesDisplay } from "./shogi-match/components/HandPiecesDisplay";
import { KifuImportPanel } from "./shogi-match/components/KifuImportPanel";
import { KifuPanel } from "./shogi-match/components/KifuPanel";
import { MatchControls } from "./shogi-match/components/MatchControls";
import {
    type EngineOption,
    MatchSettingsPanel,
    type SideSetting,
} from "./shogi-match/components/MatchSettingsPanel";
import {
    applyDropResult,
    DeleteZone,
    DragGhost,
    type DropResult,
    usePieceDnd,
} from "./shogi-match/dnd";

// EngineOption 型を外部に再エクスポート
export type { EngineOption };

import { BoardToolbar } from "./shogi-match/components/BoardToolbar";
import { DisplaySettingsPanel } from "./shogi-match/components/DisplaySettingsPanel";
import { type ClockSettings, useClockManager } from "./shogi-match/hooks/useClockManager";
import { useEngineManager } from "./shogi-match/hooks/useEngineManager";
import { useKifuKeyboardNavigation } from "./shogi-match/hooks/useKifuKeyboardNavigation";
import { useKifuNavigation } from "./shogi-match/hooks/useKifuNavigation";
import { useLocalStorage } from "./shogi-match/hooks/useLocalStorage";
import {
    DEFAULT_DISPLAY_SETTINGS,
    type DisplaySettings,
    type PromotionSelection,
} from "./shogi-match/types";
import {
    addToHand,
    cloneHandsState,
    consumeFromHand,
    countPieces,
} from "./shogi-match/utils/boardUtils";
import { isPromotable, PIECE_CAP, PIECE_LABELS } from "./shogi-match/utils/constants";
import { exportToKifString } from "./shogi-match/utils/kifFormat";
import type { KifMoveData } from "./shogi-match/utils/kifParser";
import { LegalMoveCache } from "./shogi-match/utils/legalMoveCache";
import { determinePromotion } from "./shogi-match/utils/promotionLogic";
import { TooltipProvider } from "./tooltip";

type Selection = { kind: "square"; square: string } | { kind: "hand"; piece: PieceType };

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

const baseCard: CSSProperties = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "14px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
};

// スタイル定数（保守性・一貫性のため）
const TEXT_STYLES = {
    mutedSecondary: {
        fontSize: "12px",
        color: "hsl(var(--muted-foreground, 0 0% 48%))",
    } as CSSProperties,
    handLabel: {
        fontSize: "12px",
        fontWeight: 600,
        marginBottom: "4px",
    } as CSSProperties,
    moveCount: {
        textAlign: "center",
        fontSize: "14px",
        fontWeight: 600,
        color: "hsl(var(--foreground, 0 0% 10%))",
        margin: "8px 0",
    } as CSSProperties,
} as const;

// 持ち駒表示セクションコンポーネント
interface PlayerHandSectionProps {
    owner: Player;
    hand: PositionState["hands"]["sente"] | PositionState["hands"]["gote"];
    selectedPiece: PieceType | null;
    isActive: boolean;
    onHandSelect: (piece: PieceType) => void;
    /** DnD 用 PointerDown ハンドラ */
    onPiecePointerDown?: (owner: Player, pieceType: PieceType, e: React.PointerEvent) => void;
    /** 編集モードかどうか */
    isEditMode?: boolean;
    /** 持ち駒を増やす（編集モード用） */
    onIncrement?: (piece: PieceType) => void;
    /** 持ち駒を減らす（編集モード用） */
    onDecrement?: (piece: PieceType) => void;
    /** 盤面反転状態 */
    flipBoard?: boolean;
}

function PlayerHandSection({
    owner,
    hand,
    selectedPiece,
    isActive,
    onHandSelect,
    onPiecePointerDown,
    isEditMode,
    onIncrement,
    onDecrement,
    flipBoard,
}: PlayerHandSectionProps): ReactElement {
    const labelColor = owner === "sente" ? "hsl(var(--wafuu-shu))" : "hsl(210 70% 45%)";
    const ownerText = owner === "sente" ? "先手" : "後手";
    return (
        <div data-zone={`hand-${owner}`}>
            <div style={TEXT_STYLES.handLabel}>
                <span style={{ color: labelColor, fontSize: "15px" }}>{ownerText}</span>
                <span>の持ち駒</span>
            </div>
            <HandPiecesDisplay
                owner={owner}
                hand={hand}
                selectedPiece={selectedPiece}
                isActive={isActive}
                onHandSelect={onHandSelect}
                onPiecePointerDown={onPiecePointerDown}
                isEditMode={isEditMode}
                onIncrement={onIncrement}
                onDecrement={onDecrement}
                flipBoard={flipBoard}
            />
        </div>
    );
}

const clonePositionState = (pos: PositionState): PositionState => ({
    board: cloneBoard(pos.board),
    hands: cloneHandsState(pos.hands),
    turn: pos.turn,
    ply: pos.ply,
});

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

/**
 * USI形式の指し手文字列から最終手情報を導出
 */
function deriveLastMove(move: string | undefined): LastMove | undefined {
    const parsed = move ? parseMove(move) : null;
    if (!parsed) return undefined;
    if (parsed.kind === "drop") {
        return { from: null, to: parsed.to, dropPiece: parsed.piece, promotes: false };
    }
    return { from: parsed.from, to: parsed.to, promotes: parsed.promote };
}

export function ShogiMatch({
    engineOptions,
    defaultSides = {
        sente: { role: "engine", engineId: engineOptions[0]?.id },
        gote: { role: "engine", engineId: engineOptions[0]?.id },
    },
    initialMainTimeMs = 0,
    initialByoyomiMs = DEFAULT_BYOYOMI_MS,
    maxLogs = DEFAULT_MAX_LOGS,
    fetchLegalMoves,
}: ShogiMatchProps): ReactElement {
    const emptyBoard = useMemo<BoardState>(
        () => Object.fromEntries(getAllSquares().map((sq) => [sq, null])) as BoardState,
        [],
    );
    const [sides, setSides] = useLocalStorage<{ sente: SideSetting; gote: SideSetting }>(
        "shogi-match-sides",
        defaultSides,
    );
    const [position, setPosition] = useState<PositionState>({
        board: emptyBoard,
        hands: createEmptyHands(),
        turn: "sente",
        ply: 1,
    });
    const [, setInitialBoard] = useState<BoardState | null>(null);
    const [positionReady, setPositionReady] = useState(false);
    const [lastMove, setLastMove] = useState<LastMove | undefined>(undefined);
    const [selection, setSelection] = useState<Selection | null>(null);
    const [promotionSelection, setPromotionSelection] = useState<PromotionSelection | null>(null);
    const [message, setMessage] = useState<string | null>(null);
    const [gameResult, setGameResult] = useState<GameResult | null>(null);
    const [showResultDialog, setShowResultDialog] = useState(false);
    const [showResultBanner, setShowResultBanner] = useState(false);
    const [editMessage, setEditMessage] = useState<string | null>(null);
    const [flipBoard, setFlipBoard] = useState(false);
    const [timeSettings, setTimeSettings] = useLocalStorage<ClockSettings>(
        "shogi-match-time-settings",
        {
            sente: { mainMs: initialMainTimeMs, byoyomiMs: initialByoyomiMs },
            gote: { mainMs: initialMainTimeMs, byoyomiMs: initialByoyomiMs },
        },
    );
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
    const [isDisplaySettingsPanelOpen, setIsDisplaySettingsPanelOpen] = useState(false);
    const [displaySettings, setDisplaySettings] = useLocalStorage<DisplaySettings>(
        "shogi-display-settings",
        DEFAULT_DISPLAY_SETTINGS,
    );

    // positionRef を先に定義（コールバックで使用するため）
    const positionRef = useRef<PositionState>(position);

    // ナビゲーションからの局面変更コールバック（メモ化して安定した参照を維持）
    const handleNavigationPositionChange = useCallback(
        (newPosition: PositionState, lastMoveInfo?: { from?: string; to: string }) => {
            setPosition(newPosition);
            positionRef.current = newPosition;
            // ナビゲーションからのlastMove情報を反映
            if (lastMoveInfo) {
                setLastMove({
                    from: (lastMoveInfo.from ?? null) as Square | null,
                    to: lastMoveInfo.to as Square,
                    promotes: false, // ナビゲーションでは成り情報を追跡しない
                });
            } else {
                setLastMove(undefined);
            }
        },
        [],
    );

    // 棋譜ナビゲーション管理フック
    const navigation = useKifuNavigation({
        initialPosition: position,
        initialSfen: startSfen,
        onPositionChange: handleNavigationPositionChange,
    });

    // 互換性用のmoves配列
    const moves = navigation.getMovesArray();

    // 棋譜＋評価値データ
    const { kifMoves, evalHistory, boardHistory, branchMarkers, recordEval } = navigation;

    // 後手が人間の場合は盤面を反転して手前側に表示
    useEffect(() => {
        const goteIsHuman = sides.gote.role === "human";
        const senteIsHuman = sides.sente.role === "human";
        // 後手のみ人間、または両方人間で後手優先の場合は反転
        // （後手が人間かつ先手がエンジンの場合に反転）
        setFlipBoard(goteIsHuman && !senteIsHuman);
    }, [sides.sente.role, sides.gote.role]);

    // 持ち駒表示用のヘルパー関数
    const getHandInfo = (pos: "top" | "bottom") => {
        const owner: Player =
            pos === "top" ? (flipBoard ? "sente" : "gote") : flipBoard ? "gote" : "sente";
        return {
            owner,
            hand: owner === "sente" ? position.hands.sente : position.hands.gote,
            isActive: !isEditMode && position.turn === owner && sides[owner].role === "human",
        };
    };

    const movesRef = useRef<string[]>(moves);
    // movesRefをnavigationの変更に同期
    useEffect(() => {
        movesRef.current = moves;
    }, [moves]);
    const legalCache = useMemo(() => new LegalMoveCache(), []);
    const matchEndedRef = useRef(false);
    const boardSectionRef = useRef<HTMLDivElement>(null);
    const settingsLocked = isMatchRunning;
    // 現在のターン開始時刻（消費時間計算用）
    const turnStartTimeRef = useRef<number>(Date.now());

    // endMatch のための ref（循環依存を回避）
    const endMatchRef = useRef<((result: GameResult) => Promise<void>) | null>(null);

    const handleClockError = useCallback((message: string) => {
        setMessage(message);
    }, []);

    const stopAllEnginesRef = useRef<() => Promise<void>>(async () => {});

    // 時計管理フックを使用
    const { clocks, resetClocks, updateClocksForNextTurn, stopTicking, startTicking } =
        useClockManager({
            timeSettings,
            isMatchRunning,
            onTimeExpired: async (side) => {
                const winner: Player = side === "sente" ? "gote" : "sente";
                const result: GameResult = {
                    winner,
                    reason: { kind: "time_expired", loser: side },
                    totalMoves: movesRef.current.length,
                };
                await endMatchRef.current?.(result);
            },
            matchEndedRef,
            onClockError: handleClockError,
        });

    // 対局前に timeSettings が変更されたら clocks を同期
    // （resetClocks は timeSettings に依存しているため、resetClocks の変更で検知可能）
    useEffect(() => {
        if (!isMatchRunning) {
            resetClocks(false);
        }
    }, [isMatchRunning, resetClocks]);

    // 対局終了処理（エンジン管理フックから呼ばれる）
    const endMatch = useCallback(
        async (result: GameResult) => {
            if (matchEndedRef.current) return;
            matchEndedRef.current = true;
            setGameResult(result);
            setShowResultDialog(true);
            setShowResultBanner(false);
            setIsMatchRunning(false);
            stopTicking();
            try {
                await stopAllEnginesRef.current();
            } catch (error) {
                console.error("エンジン停止に失敗しました:", error);
                setMessage(
                    `対局終了処理でエンジン停止に失敗しました: ${String(error ?? "unknown")}`,
                );
            }
        },
        [stopTicking],
    );

    // endMatchRef を更新
    endMatchRef.current = endMatch;

    const handleMoveFromEngineRef = useRef<(move: string) => void>(() => {});

    // エンジン管理フックを使用
    const {
        eventLogs,
        errorLogs,
        stopAllEngines,
        isEngineTurn,
        logEngineError,
        engineErrorDetails,
        retryEngine,
    } = useEngineManager({
        sides,
        engineOptions,
        timeSettings,
        startSfen,
        movesRef,
        positionRef,
        isMatchRunning,
        positionReady,
        onMoveFromEngine: (move) => handleMoveFromEngineRef.current(move),
        onMatchEnd: endMatch,
        onEvalUpdate: recordEval,
        maxLogs,
    });
    stopAllEnginesRef.current = stopAllEngines;

    // キーボード・ホイールナビゲーション（対局中は無効）
    useKifuKeyboardNavigation({
        onForward: navigation.goForward,
        onBack: navigation.goBack,
        onToStart: navigation.goToStart,
        onToEnd: navigation.goToEnd,
        disabled: isMatchRunning,
        containerRef: boardSectionRef,
    });

    // エンジンからの手を受け取って適用するコールバック
    const handleMoveFromEngine = useCallback(
        (move: string) => {
            if (matchEndedRef.current) return;
            const result = applyMoveWithState(positionRef.current, move, {
                validateTurn: false,
            });
            if (!result.ok) {
                logEngineError(
                    `engine move rejected (${move || "empty"}): ${result.error ?? "unknown"}`,
                );
                return;
            }
            // 消費時間を計算
            const elapsedMs = Date.now() - turnStartTimeRef.current;
            // 棋譜ナビゲーションに手を追加（局面更新はonPositionChangeで自動実行）
            navigation.addMove(move, result.next, { elapsedMs });
            movesRef.current = [...movesRef.current, move];
            setLastMove(result.lastMove);
            setSelection(null);
            setMessage(null);
            legalCache.clear();
            // ターン開始時刻をリセット
            turnStartTimeRef.current = Date.now();
            updateClocksForNextTurn(result.next.turn);
        },
        [legalCache, logEngineError, navigation, updateClocksForNextTurn],
    );
    handleMoveFromEngineRef.current = handleMoveFromEngine;

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
                let sfen = "startpos";
                try {
                    sfen = await service.boardToSfen(pos);
                    if (!cancelled) {
                        setStartSfen(sfen);
                    }
                } catch (error) {
                    if (!cancelled) {
                        setMessage(`局面のSFEN変換に失敗しました: ${String(error)}`);
                    }
                }
                // 棋譜ナビゲーションを正しい初期局面でリセット
                if (!cancelled) {
                    navigation.reset(pos, sfen);
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
        // eslint-disable-next-line react-hooks/exhaustive-deps -- navigation.resetは初回のみ使用
    }, [navigation.reset]);

    const grid = useMemo(() => {
        const g = boardToGrid(position.board);
        return flipBoard ? [...g].reverse().map((row) => [...row].reverse()) : g;
    }, [position.board, flipBoard]);

    const refreshStartSfen = useCallback(async (pos: PositionState) => {
        try {
            const sfen = await getPositionService().boardToSfen(pos);
            setStartSfen(sfen);
        } catch (error) {
            setMessage(`局面のSFEN変換に失敗しました: ${String(error)}`);
            throw error;
        }
    }, []);

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

        // エンジン管理は useEngineManager フックが自動的に処理する
        setIsMatchRunning(true);
        // ターン開始時刻をリセット
        turnStartTimeRef.current = Date.now();
        startTicking(position.turn);
    };

    const finalizeEditedPosition = async () => {
        if (isMatchRunning) return;
        const current = positionRef.current;
        setBasePosition(clonePositionState(current));
        setInitialBoard(cloneBoard(current.board));
        await refreshStartSfen(current);
        legalCache.clear();
        setIsEditMode(false);
        setEditMessage("局面を確定しました。対局開始でこの局面から進行します。");
    };

    const resetToBasePosition = useCallback(async () => {
        matchEndedRef.current = false;
        setGameResult(null);
        setShowResultDialog(false);
        setShowResultBanner(false);
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
        // 棋譜ナビゲーションをリセット（startSfenは後でrefreshStartSfenで更新される）
        navigation.reset(next, startSfen);
        movesRef.current = [];
        setLastMove(undefined);
        setSelection(null);
        setMessage(null);
        resetClocks(false);

        setIsMatchRunning(false);
        setIsEditMode(true);
        setEditFromSquare(null);
        setEditTool("place");
        setEditPromoted(false);
        setEditOwner("sente");
        setEditPieceType(null);
        legalCache.clear();
        // ターン開始時刻をリセット
        turnStartTimeRef.current = Date.now();
        void refreshStartSfen(next);
    }, [
        basePosition,
        navigation,
        startSfen,
        refreshStartSfen,
        resetClocks,
        stopAllEngines,
        legalCache.clear,
    ]);

    const applyMoveCommon = useCallback(
        (nextPosition: PositionState, mv: string, last?: LastMove, _prevBoard?: BoardState) => {
            // 消費時間を計算
            const elapsedMs = Date.now() - turnStartTimeRef.current;
            // 棋譜ナビゲーションに手を追加（局面更新はonPositionChangeで自動実行）
            navigation.addMove(mv, nextPosition, { elapsedMs });
            movesRef.current = [...movesRef.current, mv];
            setLastMove(last);
            setSelection(null);
            setMessage(null);
            legalCache.clear();
            // ターン開始時刻をリセット
            turnStartTimeRef.current = Date.now();
            updateClocksForNextTurn(nextPosition.turn);
        },
        [legalCache, navigation, updateClocksForNextTurn],
    );

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

    const applyEditedPosition = useCallback(
        (nextPosition: PositionState) => {
            setPosition(nextPosition);
            positionRef.current = nextPosition;
            setInitialBoard(cloneBoard(nextPosition.board));
            // 棋譜ナビゲーションをリセット（startSfenは後でrefreshStartSfenで更新される）
            navigation.reset(nextPosition, startSfen);
            movesRef.current = [];
            setLastMove(undefined);
            setSelection(null);
            setMessage(null);
            setEditFromSquare(null);

            legalCache.clear();
            stopTicking();
            matchEndedRef.current = false;
            setIsMatchRunning(false);
            void refreshStartSfen(nextPosition);
        },
        [navigation, startSfen, legalCache, stopTicking, refreshStartSfen],
    );

    const setPiecePromotion = useCallback(
        (square: Square, promote: boolean) => {
            if (!isEditMode) return;
            const current = positionRef.current;
            const piece = current.board[square];
            if (!piece) return;
            if (!isPromotable(piece.type)) {
                setEditMessage(`${PIECE_LABELS[piece.type]}は成れません。`);
                return;
            }

            const nextBoard = cloneBoard(current.board);
            nextBoard[square] = promote
                ? { ...piece, promoted: true }
                : { ...piece, promoted: undefined };
            applyEditedPosition({ ...current, board: nextBoard });
        },
        [applyEditedPosition, isEditMode],
    );

    // DnD ドロップハンドラ
    const handleDndDrop = useCallback(
        (result: DropResult) => {
            if (!isEditMode) return;

            const applied = applyDropResult(result, positionRef.current);
            if (!applied.ok) {
                setMessage(applied.error ?? "ドロップに失敗しました");
                return;
            }

            applyEditedPosition(applied.next);
        },
        [isEditMode, applyEditedPosition],
    );

    // DnD コントローラー
    const dndController = usePieceDnd({
        onDrop: handleDndDrop,
        disabled: !isEditMode,
    });

    // DnD ドラッグ開始ハンドラ（盤上の駒）
    // 注: isEditMode チェックは usePieceDnd の disabled オプションと
    //     JSX での条件付き props 渡しで行うため、ここでは不要
    const handlePiecePointerDown = useCallback(
        (
            square: string,
            piece: { owner: "sente" | "gote"; type: string; promoted?: boolean },
            e: React.PointerEvent,
        ) => {
            // 編集パネルが閉じていたら自動的に開く
            if (!isEditPanelOpen) {
                setIsEditPanelOpen(true);
            }

            const origin = { type: "board" as const, square: square as Square };
            const payload = {
                owner: piece.owner as Player,
                pieceType: piece.type as PieceType,
                isPromoted: piece.promoted ?? false,
            };

            dndController.startDrag(origin, payload, e);
        },
        [dndController, isEditPanelOpen],
    );

    // DnD ドラッグ開始ハンドラ（持ち駒）
    const handleHandPiecePointerDown = useCallback(
        (owner: Player, pieceType: PieceType, e: React.PointerEvent) => {
            // 編集パネルが閉じていたら自動的に開く
            if (!isEditPanelOpen) {
                setIsEditPanelOpen(true);
            }

            // 持ち駒が0個の場合はストック扱い（編集モード時、無限供給）
            const count = position?.hands[owner][pieceType] ?? 0;
            const origin =
                count > 0
                    ? { type: "hand" as const, owner, pieceType }
                    : { type: "stock" as const, owner, pieceType };
            const payload = {
                owner,
                pieceType,
                isPromoted: false,
            };

            dndController.startDrag(origin, payload, e);
        },
        [dndController, position, isEditPanelOpen],
    );

    const handlePieceTogglePromote = useCallback(
        (
            square: string,
            piece: { owner: "sente" | "gote"; type: string; promoted?: boolean },
            _event: React.MouseEvent<HTMLButtonElement>,
        ) => {
            if (!isEditMode) return;
            const sq = square as Square;
            setPiecePromotion(sq, !piece.promoted);
        },
        [isEditMode, setPiecePromotion],
    );

    // 持ち駒増加ハンドラ（編集モード用）
    const handleIncrementHand = useCallback(
        (owner: Player, pieceType: PieceType) => {
            if (isMatchRunning || !position) return;
            const counts = countPieces(position);
            const currentCount = counts[owner][pieceType];
            if (currentCount >= PIECE_CAP[pieceType]) return;

            const nextHands = addToHand(cloneHandsState(position.hands), owner, pieceType);
            setPosition({
                ...position,
                hands: nextHands,
            });
        },
        [isMatchRunning, position],
    );

    // 持ち駒減少ハンドラ（編集モード用）
    const handleDecrementHand = useCallback(
        (owner: Player, pieceType: PieceType) => {
            if (isMatchRunning || !position) return;
            const count = position.hands[owner][pieceType] ?? 0;
            if (count <= 0) return;

            const nextHands = consumeFromHand(cloneHandsState(position.hands), owner, pieceType);
            if (nextHands) {
                setPosition({
                    ...position,
                    hands: nextHands,
                });
            }
        },
        [isMatchRunning, position],
    );

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
        setEditMessage("盤面をクリアしました。");
    };

    const resetToStartposForEdit = async () => {
        if (isMatchRunning) return;
        try {
            const service = getPositionService();
            const pos = await service.getInitialBoard();
            applyEditedPosition(clonePositionState(pos));
            setInitialBoard(cloneBoard(pos.board));
            setEditMessage("平手初期化しました。");
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
            setEditMessage(
                `${piece.owner === "sente" ? "先手" : "後手"}の${PIECE_LABELS[baseType]}は最大${PIECE_CAP[baseType]}枚までです`,
            );
            return false;
        }
        if (piece.type === "K" && countsBefore[piece.owner][baseType] >= PIECE_CAP.K) {
            setEditMessage("玉はそれぞれ1枚まで配置できます。");
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
            setEditMessage("配置する駒を選ぶか、移動する駒をクリックしてください。");
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
                const prevBoard = position.board;
                const result = applyMoveWithState(position, moveStr, { validateTurn: true });
                if (!result.ok) {
                    setMessage(result.error ?? "指し手を適用できませんでした");
                    return;
                }
                applyMoveCommon(result.next, moveStr, result.lastMove, prevBoard);
                return;
            }

            // 【ケース2】強制成り → 自動的に成って移動（ダイアログなし）
            if (promotion === "forced") {
                const moveStr = `${from}${to}+`;
                const prevBoard = position.board;
                const result = applyMoveWithState(position, moveStr, { validateTurn: true });
                if (!result.ok) {
                    setMessage(result.error ?? "指し手を適用できませんでした");
                    return;
                }
                applyMoveCommon(result.next, moveStr, result.lastMove, prevBoard);
                return;
            }

            // 【ケース3】任意成り（promotion === 'optional'）
            // Shift+クリック：即座に成って移動
            if (shiftKey) {
                const moveStr = `${from}${to}+`;
                const prevBoard = position.board;
                const result = applyMoveWithState(position, moveStr, { validateTurn: true });
                if (!result.ok) {
                    setMessage(result.error ?? "指し手を適用できませんでした");
                    return;
                }
                applyMoveCommon(result.next, moveStr, result.lastMove, prevBoard);
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
        const prevBoard = position.board;
        const result = applyMoveWithState(position, moveStr, { validateTurn: true });
        if (!result.ok) {
            setMessage(result.error ?? "持ち駒を打てませんでした");
            return;
        }
        applyMoveCommon(result.next, moveStr, result.lastMove, prevBoard);
    };

    const handlePromotionChoice = (promote: boolean) => {
        if (!promotionSelection) return;
        const { from, to } = promotionSelection;
        const moveStr = `${from}${to}${promote ? "+" : ""}`;
        const prevBoard = position.board;
        const result = applyMoveWithState(position, moveStr, { validateTurn: true });
        if (!result.ok) {
            setMessage(result.error ?? "指し手を適用できませんでした");
            setPromotionSelection(null);
            setSelection(null);
            return;
        }
        applyMoveCommon(result.next, moveStr, result.lastMove, prevBoard);
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

    const loadMoves = useCallback(
        async (list: string[], moveData?: KifMoveData[]) => {
            const filtered = list.filter(Boolean);
            const service = getPositionService();
            try {
                const result = await service.replayMovesStrict(startSfen, filtered);

                // 開始局面を取得
                const startPosition = basePosition ?? (await service.getInitialBoard());

                // 棋譜ナビゲーションをリセット
                navigation.reset(startPosition, startSfen);

                // 各手を順番に追加
                let currentPos = startPosition;
                for (let i = 0; i < result.applied.length; i++) {
                    const move = result.applied[i];
                    const data = moveData?.[i];
                    const applyResult = applyMoveWithState(currentPos, move, {
                        validateTurn: false,
                    });
                    if (applyResult.ok) {
                        // 消費時間と評価値を渡す
                        // KIFインポートの評価値は既に先手視点なので normalized: true
                        navigation.addMove(move, applyResult.next, {
                            elapsedMs: data?.elapsedMs,
                            eval:
                                data?.evalCp !== undefined || data?.evalMate !== undefined
                                    ? {
                                          scoreCp: data.evalCp,
                                          scoreMate: data.evalMate,
                                          depth: data.depth,
                                          normalized: true,
                                      }
                                    : undefined,
                        });
                        currentPos = applyResult.next;
                    }
                }

                movesRef.current = result.applied;
                setLastMove(deriveLastMove(result.applied.at(-1)));
                setSelection(null);
                setMessage(result.error ?? null);
                resetClocks(false);

                legalCache.clear();
                setPositionReady(true);
            } catch (error) {
                setMessage(`棋譜の適用に失敗しました: ${String(error)}`);
            }
        },
        [startSfen, basePosition, navigation, resetClocks, legalCache],
    );

    // KIFコピー用コールバック
    const handleCopyKif = useCallback((): string => {
        return exportToKifString(kifMoves, boardHistory, {
            startTime: new Date(),
            senteName: sides.sente.role === "engine" ? "エンジン" : "人間",
            goteName: sides.gote.role === "engine" ? "エンジン" : "人間",
            includeEval: true, // 評価値もコメントとして出力
        });
    }, [kifMoves, boardHistory, sides.sente.role, sides.gote.role]);

    // 棋譜の手数選択コールバック（巻き戻し・リプレイ用）
    const handlePlySelect = useCallback(
        (ply: number) => {
            // 対局中は自動進行を一時停止
            if (isMatchRunning) {
                setIsMatchRunning(false);
                stopTicking();
                void stopAllEngines();
            }
            // 指定手数に移動（lastMoveはonPositionChangeで自動設定される）
            navigation.goToPly(ply);
        },
        [isMatchRunning, navigation, stopTicking, stopAllEngines],
    );

    // SFENインポート（局面 + 指し手）
    const importSfen = useCallback(
        async (sfen: string, movesToLoad: string[]) => {
            const service = getPositionService();
            try {
                // 新しい開始局面を設定
                const newPosition = await service.parseSfen(sfen);
                setBasePosition(newPosition);
                setStartSfen(sfen);
                setInitialBoard(newPosition.board);

                // 棋譜ナビゲーションをリセット
                navigation.reset(newPosition, sfen);

                // 指し手がある場合は適用
                if (movesToLoad.length > 0) {
                    let currentPos = newPosition;
                    const appliedMoves: string[] = [];
                    for (const move of movesToLoad) {
                        const applyResult = applyMoveWithState(currentPos, move, {
                            validateTurn: false,
                        });
                        if (applyResult.ok) {
                            navigation.addMove(move, applyResult.next);
                            currentPos = applyResult.next;
                            appliedMoves.push(move);
                        } else {
                            break;
                        }
                    }
                    movesRef.current = appliedMoves;
                    setLastMove(deriveLastMove(appliedMoves.at(-1)));
                } else {
                    movesRef.current = [];
                    setLastMove(undefined);
                }

                setSelection(null);
                setMessage(null);
                resetClocks(false);
                legalCache.clear();
                setPositionReady(true);
            } catch (error) {
                setMessage(`SFENの適用に失敗しました: ${String(error)}`);
            }
        },
        [navigation, resetClocks, legalCache],
    );

    // KIFインポート（指し手のみ、平手初期局面から）
    const importKif = useCallback(
        async (movesToLoad: string[], moveData: KifMoveData[]) => {
            await loadMoves(movesToLoad, moveData);
        },
        [loadMoves],
    );

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
            {/* DnD ゴースト */}
            <DragGhost
                ref={dndController.ghostRef as React.RefObject<HTMLDivElement>}
                dndState={dndController.state}
                ownerOrientation={flipBoard ? "gote" : "sente"}
            />

            {/* 勝敗表示ダイアログ */}
            <GameResultDialog
                result={gameResult}
                open={showResultDialog}
                onClose={() => {
                    setShowResultDialog(false);
                    setShowResultBanner(true);
                }}
            />

            <section
                style={{
                    display: "flex",
                    flexDirection: "column",
                    gap: "12px",
                    alignItems: "center",
                    padding: "16px 0",
                }}
            >
                {/* 勝敗表示バナー */}
                <GameResultBanner
                    result={gameResult}
                    visible={showResultBanner}
                    onShowDetail={() => {
                        setShowResultDialog(true);
                        setShowResultBanner(false);
                    }}
                    onClose={() => setShowResultBanner(false)}
                />

                <div
                    style={{
                        display: "flex",
                        gap: "24px",
                        alignItems: "flex-start",
                    }}
                >
                    <div
                        style={{
                            display: "flex",
                            flexDirection: "column",
                            gap: "12px",
                            alignItems: "center",
                        }}
                    >
                        <div
                            ref={boardSectionRef}
                            style={{ ...baseCard, padding: "12px", width: "fit-content" }}
                        >
                            <div
                                style={{
                                    display: "flex",
                                    justifyContent: "flex-start",
                                    alignItems: "center",
                                    marginBottom: "8px",
                                }}
                            >
                                <BoardToolbar
                                    flipBoard={flipBoard}
                                    onFlipBoardChange={setFlipBoard}
                                    displaySettings={displaySettings}
                                    onDisplaySettingsChange={setDisplaySettings}
                                />
                            </div>
                            <div
                                style={{
                                    marginTop: "8px",
                                    display: "flex",
                                    gap: "8px",
                                    flexDirection: "column",
                                    alignItems: "center",
                                    touchAction:
                                        isEditMode && dndController.state.isDragging
                                            ? "none"
                                            : "auto",
                                }}
                            >
                                {/* 盤の上側の持ち駒（通常:後手、反転時:先手） */}
                                {(() => {
                                    const info = getHandInfo("top");
                                    const labelColor =
                                        info.owner === "sente"
                                            ? "hsl(var(--wafuu-shu))"
                                            : "hsl(210 70% 45%)";
                                    const ownerText = info.owner === "sente" ? "先手" : "後手";
                                    return (
                                        <div data-zone={`hand-${info.owner}`}>
                                            {/* ラベル行: [持ち駒ラベル] [手数] [手番] */}
                                            <div
                                                style={{
                                                    display: "flex",
                                                    alignItems: "center",
                                                    justifyContent: "space-between",
                                                    marginBottom: "4px",
                                                    gap: "16px",
                                                }}
                                            >
                                                {/* 持ち駒ラベル（左） */}
                                                <div
                                                    style={{
                                                        ...TEXT_STYLES.handLabel,
                                                        marginBottom: 0,
                                                        whiteSpace: "nowrap",
                                                    }}
                                                >
                                                    <span
                                                        style={{
                                                            color: labelColor,
                                                            fontSize: "15px",
                                                        }}
                                                    >
                                                        {ownerText}
                                                    </span>
                                                    <span>の持ち駒</span>
                                                </div>

                                                {/* 手数表示（中央） */}
                                                <output
                                                    style={{
                                                        ...TEXT_STYLES.moveCount,
                                                        margin: 0,
                                                        whiteSpace: "nowrap",
                                                    }}
                                                >
                                                    {moves.length === 0
                                                        ? "開始局面"
                                                        : `${moves.length}手目`}
                                                </output>

                                                {/* 手番表示（右） */}
                                                <output
                                                    style={{
                                                        ...TEXT_STYLES.mutedSecondary,
                                                        whiteSpace: "nowrap",
                                                    }}
                                                >
                                                    手番:{" "}
                                                    <span
                                                        style={{
                                                            fontWeight: 600,
                                                            fontSize: "15px",
                                                            color:
                                                                position.turn === "sente"
                                                                    ? "hsl(var(--wafuu-shu))"
                                                                    : "hsl(210 70% 45%)",
                                                        }}
                                                    >
                                                        {position.turn === "sente"
                                                            ? "先手"
                                                            : "後手"}
                                                    </span>
                                                </output>
                                            </div>

                                            {/* 持ち駒表示 */}
                                            <HandPiecesDisplay
                                                owner={info.owner}
                                                hand={info.hand}
                                                selectedPiece={
                                                    selection?.kind === "hand"
                                                        ? selection.piece
                                                        : null
                                                }
                                                isActive={info.isActive}
                                                onHandSelect={handleHandSelect}
                                                onPiecePointerDown={
                                                    isEditMode
                                                        ? handleHandPiecePointerDown
                                                        : undefined
                                                }
                                                isEditMode={isEditMode && !isMatchRunning}
                                                onIncrement={(piece) =>
                                                    handleIncrementHand(info.owner, piece)
                                                }
                                                onDecrement={(piece) =>
                                                    handleDecrementHand(info.owner, piece)
                                                }
                                                flipBoard={flipBoard}
                                            />
                                        </div>
                                    );
                                })()}

                                {/* 盤面 */}
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
                                        displaySettings.highlightLastMove && lastMove
                                            ? {
                                                  from: lastMove.from ?? undefined,
                                                  to: lastMove.to,
                                              }
                                            : undefined
                                    }
                                    promotionSquare={promotionSelection?.to ?? null}
                                    onSelect={(sq, shiftKey) => {
                                        void handleSquareSelect(sq, shiftKey);
                                    }}
                                    onPromotionChoice={handlePromotionChoice}
                                    flipBoard={flipBoard}
                                    onPiecePointerDown={
                                        isEditMode ? handlePiecePointerDown : undefined
                                    }
                                    onPieceTogglePromote={
                                        isEditMode ? handlePieceTogglePromote : undefined
                                    }
                                    squareNotation={displaySettings.squareNotation}
                                    showBoardLabels={displaySettings.showBoardLabels}
                                />
                                {candidateNote ? (
                                    <div style={TEXT_STYLES.mutedSecondary}>{candidateNote}</div>
                                ) : null}

                                {/* 盤の下側の持ち駒（通常:先手、反転時:後手） */}
                                {(() => {
                                    const info = getHandInfo("bottom");
                                    return (
                                        <PlayerHandSection
                                            owner={info.owner}
                                            hand={info.hand}
                                            selectedPiece={
                                                selection?.kind === "hand" ? selection.piece : null
                                            }
                                            isActive={info.isActive}
                                            onHandSelect={handleHandSelect}
                                            onPiecePointerDown={
                                                isEditMode ? handleHandPiecePointerDown : undefined
                                            }
                                            isEditMode={isEditMode && !isMatchRunning}
                                            onIncrement={(piece) =>
                                                handleIncrementHand(info.owner, piece)
                                            }
                                            onDecrement={(piece) =>
                                                handleDecrementHand(info.owner, piece)
                                            }
                                            flipBoard={flipBoard}
                                        />
                                    );
                                })()}

                                {/* DnD 削除ゾーン（編集モード時のみ表示） */}
                                {isEditMode && (
                                    <DeleteZone
                                        dndState={dndController.state}
                                        className="mt-2 h-14 w-full"
                                    />
                                )}
                            </div>
                        </div>
                    </div>

                    <div style={{ display: "flex", flexDirection: "column", gap: "10px" }}>
                        <EditModePanel
                            isOpen={isEditPanelOpen}
                            onOpenChange={setIsEditPanelOpen}
                            onResetToStartpos={resetToStartposForEdit}
                            onClearBoard={clearBoardForEdit}
                            isMatchRunning={isMatchRunning}
                            positionReady={positionReady}
                            message={editMessage}
                        />

                        <MatchControls
                            onNewGame={handleNewGame}
                            onPause={pauseAutoPlay}
                            onResume={resumeAutoPlay}
                            message={message}
                        />

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

                        <ClockDisplayPanel clocks={clocks} sides={sides} />

                        {/* 棋譜パネル（常時表示） */}
                        <KifuPanel
                            kifMoves={kifMoves}
                            currentPly={navigation.state.currentPly}
                            showEval={false}
                            onPlySelect={handlePlySelect}
                            onCopyKif={handleCopyKif}
                            navigation={{
                                currentPly: navigation.state.currentPly,
                                totalPly: navigation.state.totalPly,
                                onBack: navigation.goBack,
                                onForward: navigation.goForward,
                                onToStart: navigation.goToStart,
                                onToEnd: navigation.goToEnd,
                                isRewound: navigation.state.isRewound,
                                canGoForward: navigation.state.canGoForward,
                                branchInfo: navigation.state.hasBranches
                                    ? {
                                          hasBranches: true,
                                          currentIndex: navigation.state.currentBranchIndex,
                                          count: navigation.state.branchCount,
                                          onSwitch: navigation.switchBranch,
                                          onPromoteToMain: navigation.promoteCurrentLine,
                                      }
                                    : undefined,
                            }}
                            navigationDisabled={isMatchRunning}
                            branchMarkers={branchMarkers}
                        />

                        {/* 評価値グラフパネル（折りたたみ） */}
                        <EvalPanel
                            evalHistory={evalHistory}
                            currentPly={navigation.state.currentPly}
                            onPlySelect={handlePlySelect}
                            defaultOpen={false}
                        />

                        {/* インポートパネル */}
                        <KifuImportPanel
                            onImportSfen={importSfen}
                            onImportKif={importKif}
                            positionReady={positionReady}
                        />

                        <DisplaySettingsPanel
                            isOpen={isDisplaySettingsPanelOpen}
                            onOpenChange={setIsDisplaySettingsPanelOpen}
                            settings={displaySettings}
                            onSettingsChange={setDisplaySettings}
                        />

                        <EngineLogsPanel
                            eventLogs={eventLogs}
                            errorLogs={errorLogs}
                            engineErrorDetails={engineErrorDetails}
                            onRetry={retryEngine}
                        />
                    </div>
                </div>
            </section>
        </TooltipProvider>
    );
}
