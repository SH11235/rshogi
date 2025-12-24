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
import type { CSSProperties, ReactElement } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ShogiBoardCell } from "./shogi-board";
import { ShogiBoard } from "./shogi-board";
import { ClockDisplayPanel } from "./shogi-match/components/ClockDisplayPanel";
import { EditModePanel } from "./shogi-match/components/EditModePanel";
import { EngineLogsPanel } from "./shogi-match/components/EngineLogsPanel";
import { HandPiecesDisplay } from "./shogi-match/components/HandPiecesDisplay";
import { KifuIOPanel } from "./shogi-match/components/KifuIOPanel";
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

import { type ClockSettings, useClockManager } from "./shogi-match/hooks/useClockManager";
import { useEngineManager } from "./shogi-match/hooks/useEngineManager";
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

// 持ち駒表示セクションコンポーネント（DRY原則）
interface PlayerHandSectionProps {
    owner: Player;
    label: string;
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
}

function PlayerHandSection({
    owner,
    label,
    hand,
    selectedPiece,
    isActive,
    onHandSelect,
    onPiecePointerDown,
    isEditMode,
    onIncrement,
    onDecrement,
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
    const [message, setMessage] = useState<string | null>(null);
    const [flipBoard, setFlipBoard] = useState(false);
    const [timeSettings, setTimeSettings] = useState<ClockSettings>({
        sente: { mainMs: initialMainTimeMs, byoyomiMs: initialByoyomiMs },
        gote: { mainMs: initialMainTimeMs, byoyomiMs: initialByoyomiMs },
    });
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
            label: owner === "sente" ? "先手の持ち駒" : "後手の持ち駒",
            hand: owner === "sente" ? position.hands.sente : position.hands.gote,
            isActive: !isEditMode && position.turn === owner && sides[owner].role === "human",
        };
    };

    const positionRef = useRef<PositionState>(position);
    const movesRef = useRef<string[]>(moves);
    const legalCache = useMemo(() => new LegalMoveCache(), []);
    const matchEndedRef = useRef(false);
    const boardSectionRef = useRef<HTMLDivElement>(null);
    const settingsLocked = isMatchRunning;

    // endMatch のための ref（循環依存を回避）
    const endMatchRef = useRef<((message: string) => Promise<void>) | null>(null);

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
                const loserLabel = side === "sente" ? "先手" : "後手";
                const winnerLabel = side === "sente" ? "後手" : "先手";
                await endMatchRef.current?.(
                    `対局終了: ${loserLabel}が時間切れ。${winnerLabel}の勝ち。`,
                );
            },
            matchEndedRef,
            onClockError: handleClockError,
        });

    // 対局終了処理（エンジン管理フックから呼ばれる）
    const endMatch = useCallback(
        async (nextMessage: string) => {
            if (matchEndedRef.current) return;
            matchEndedRef.current = true;
            setMessage(nextMessage);
            setIsMatchRunning(false);
            stopTicking();
            try {
                await stopAllEnginesRef.current();
            } catch (error) {
                console.error("エンジン停止に失敗しました:", error);
                setMessage(
                    (prev) =>
                        prev ??
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
        engineReady,
        engineStatus,
        eventLogs,
        errorLogs,
        stopAllEngines,
        getEngineForSide,
        isEngineTurn,
        logEngineError,
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
        maxLogs,
    });
    stopAllEnginesRef.current = stopAllEngines;

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
            // 局面を更新
            setPosition(result.next);
            positionRef.current = result.next;
            setMoves((prev) => [...prev, move]);
            movesRef.current = [...movesRef.current, move];
            setLastMove(result.lastMove);
            setSelection(null);
            setMessage(null);
            legalCache.clear();
            updateClocksForNextTurn(result.next.turn);
        },
        [legalCache, logEngineError, updateClocksForNextTurn],
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

        setIsMatchRunning(false);
        setIsEditMode(true);
        setEditFromSquare(null);
        setEditTool("place");
        setEditPromoted(false);
        setEditOwner("sente");
        setEditPieceType(null);
        legalCache.clear();
        void refreshStartSfen(next);
    }, [basePosition, refreshStartSfen, resetClocks, stopAllEngines, legalCache.clear]);

    const applyMoveCommon = useCallback(
        (nextPosition: PositionState, mv: string, last?: LastMove) => {
            setPosition(nextPosition);
            positionRef.current = nextPosition;
            setMoves((prev) => [...prev, mv]);
            movesRef.current = [...movesRef.current, mv];
            setLastMove(last);
            setSelection(null);
            setMessage(null);
            legalCache.clear();
            updateClocksForNextTurn(nextPosition.turn);
        },
        [legalCache, updateClocksForNextTurn],
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
            setMoves([]);
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
        [legalCache, stopTicking, refreshStartSfen],
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

    const importCsa = async (csa: string) => {
        if (!positionReady) return;
        await loadMoves(parseCsaMoves(csa, initialBoard ?? undefined));
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
            {/* DnD ゴースト */}
            <DragGhost
                ref={dndController.ghostRef as React.RefObject<HTMLDivElement>}
                dndState={dndController.state}
                ownerOrientation={flipBoard ? "gote" : "sente"}
            />

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
                        display: "grid",
                        gridTemplateColumns: "minmax(320px, 1fr) 360px",
                        gap: "12px",
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
                                    justifyContent: "space-between",
                                    alignItems: "center",
                                    marginBottom: "8px",
                                }}
                            >
                                <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
                                    <h3 style={{ fontWeight: 700, margin: 0, fontSize: "inherit" }}>
                                        盤面
                                    </h3>
                                    <label
                                        style={{
                                            display: "flex",
                                            alignItems: "center",
                                            gap: "4px",
                                            fontSize: "12px",
                                            color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                            cursor: "pointer",
                                        }}
                                    >
                                        <input
                                            type="checkbox"
                                            checked={flipBoard}
                                            onChange={(e) => setFlipBoard(e.target.checked)}
                                        />
                                        反転
                                    </label>
                                </div>
                                <output style={TEXT_STYLES.mutedSecondary}>
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
                                        {position.turn === "sente" ? "先手" : "後手"}
                                    </span>
                                </output>
                            </div>
                            <div
                                style={{
                                    marginTop: "8px",
                                    display: "flex",
                                    gap: "8px",
                                    flexDirection: "column",
                                }}
                            >
                                {/* 手数表示 */}
                                <output style={TEXT_STYLES.moveCount}>
                                    {moves.length === 0 ? "開始局面" : `${moves.length}手目`}
                                </output>

                                {/* 盤の上側の持ち駒（通常:後手、反転時:先手） */}
                                {(() => {
                                    const info = getHandInfo("top");
                                    return (
                                        <PlayerHandSection
                                            owner={info.owner}
                                            label={info.label}
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
                                        />
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
                                        lastMove
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
                                            label={info.label}
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
                        />

                        <MatchControls
                            onNewGame={handleNewGame}
                            onPause={pauseAutoPlay}
                            onResume={resumeAutoPlay}
                            sides={sides}
                            engineReady={engineReady}
                            engineStatus={engineStatus}
                            isMatchRunning={isMatchRunning}
                            message={message}
                            getEngineForSide={getEngineForSide}
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

                        <KifuIOPanel
                            moves={moves}
                            exportUsi={exportUsi}
                            exportCsa={exportCsa}
                            onImportUsi={importUsi}
                            onImportCsa={importCsa}
                            positionReady={positionReady}
                        />

                        <EngineLogsPanel eventLogs={eventLogs} errorLogs={errorLogs} />
                    </div>
                </div>
            </section>
        </TooltipProvider>
    );
}
