import type { LastMove, PieceType, Player, PositionState, Square } from "@shogi/app-core";
import type { ReactElement, RefObject } from "react";
import { useState } from "react";
import type { ShogiBoardCell } from "../../shogi-board";
import { ShogiBoard } from "../../shogi-board";
import { BottomSheet } from "../components/BottomSheet";
import { EvalBar } from "../components/EvalBar";
import { HandPiecesDisplay } from "../components/HandPiecesDisplay";
import type { EngineOption, SideSetting } from "../components/MatchSettingsPanel";
import { MobileClockDisplay } from "../components/MobileClockDisplay";
import { type KifuMove, MobileKifuBar } from "../components/MobileKifuBar";
import { MobileNavigation } from "../components/MobileNavigation";
import { MobileSettingsSheet } from "../components/MobileSettingsSheet";
import type { ClockSettings, TickState } from "../hooks/useClockManager";
import { useMobileCellSize } from "../hooks/useMobileCellSize";
import type { DisplaySettings, GameMode, PromotionSelection } from "../types";

// テキストスタイル用Tailwindクラス
const TEXT_CLASSES = {
    mutedSecondary: "text-xs text-muted-foreground",
    moveCount: "text-center text-sm font-semibold text-foreground",
} as const;

type Selection = { kind: "square"; square: string } | { kind: "hand"; piece: PieceType };

export interface MobileLayoutProps {
    // 盤面関連
    grid: ShogiBoardCell[][];
    position: PositionState;
    flipBoard: boolean;
    lastMove?: LastMove;
    selection: Selection | null;
    promotionSelection: PromotionSelection | null;
    isEditMode: boolean;
    isMatchRunning: boolean;
    gameMode: GameMode;
    editFromSquare: Square | null;
    moves: string[];
    candidateNote: string | null;

    // 表示設定
    displaySettings: Pick<
        DisplaySettings,
        "highlightLastMove" | "squareNotation" | "showBoardLabels"
    >;

    // イベントハンドラ
    onSquareSelect: (square: string, shiftKey?: boolean) => void;
    onPromotionChoice: (promote: boolean) => void;
    onFlipBoard: () => void;
    onHandSelect: (piece: PieceType) => void;

    // 編集モード用
    onPiecePointerDown?: (
        square: string,
        piece: { owner: "sente" | "gote"; type: string; promoted?: boolean },
        e: React.PointerEvent,
    ) => void;
    onPieceTogglePromote?: (
        square: string,
        piece: { owner: "sente" | "gote"; type: string; promoted?: boolean },
        event: React.MouseEvent<HTMLButtonElement>,
    ) => void;
    onHandPiecePointerDown?: (owner: Player, pieceType: PieceType, e: React.PointerEvent) => void;
    onIncrementHand?: (owner: Player, piece: PieceType) => void;
    onDecrementHand?: (owner: Player, piece: PieceType) => void;

    // 検討モード関連
    isReviewMode: boolean;

    // 棋譜関連
    kifMoves?: KifuMove[];
    currentPly?: number;
    totalPly?: number;
    onPlySelect?: (ply: number) => void;

    // ナビゲーション
    onBack?: () => void;
    onForward?: () => void;
    onToStart?: () => void;
    onToEnd?: () => void;

    // 評価値
    evalCp?: number;
    evalMate?: number;

    // 対局コントロール
    onStop?: () => void;
    onStart?: () => void;
    onResetToStartpos?: () => void;
    onStartReview?: () => void;

    // 対局設定（モバイル用BottomSheet）
    sides: { sente: SideSetting; gote: SideSetting };
    onSidesChange: (sides: { sente: SideSetting; gote: SideSetting }) => void;
    timeSettings: ClockSettings;
    onTimeSettingsChange: (settings: ClockSettings) => void;
    onTurnChange: (turn: Player) => void;
    uiEngineOptions: EngineOption[];
    settingsLocked: boolean;

    // クロック表示
    clocks: TickState;

    // 表示設定（フル版、BottomSheet用）
    displaySettingsFull: DisplaySettings;
    onDisplaySettingsChange: (settings: DisplaySettings) => void;

    // 持ち駒情報取得
    getHandInfo: (pos: "top" | "bottom") => {
        owner: Player;
        hand: PositionState["hands"]["sente"] | PositionState["hands"]["gote"];
        isActive: boolean;
    };

    // Ref
    boardSectionRef: RefObject<HTMLDivElement | null>;

    // DnD関連
    isDraggingPiece: boolean;
}

/**
 * スマホ用レイアウト
 * 対局モード: 盤面フルサイズ + 最小限UI
 * 検討モード: 盤面縮小 + 棋譜パネル
 */
export function MobileLayout({
    grid,
    position,
    flipBoard,
    lastMove,
    selection,
    promotionSelection,
    isEditMode,
    isMatchRunning,
    gameMode,
    editFromSquare,
    moves,
    candidateNote,
    displaySettings,
    onSquareSelect,
    onPromotionChoice,
    onFlipBoard,
    onHandSelect,
    onPiecePointerDown,
    onPieceTogglePromote,
    onHandPiecePointerDown,
    onIncrementHand,
    onDecrementHand,
    isReviewMode,
    kifMoves,
    currentPly = 0,
    totalPly = 0,
    onPlySelect,
    onBack,
    onForward,
    onToStart,
    onToEnd,
    evalCp,
    evalMate,
    onStop,
    onResetToStartpos,
    onStartReview,
    sides,
    onSidesChange,
    timeSettings,
    onTimeSettingsChange,
    onTurnChange,
    uiEngineOptions,
    settingsLocked,
    clocks,
    displaySettingsFull,
    onDisplaySettingsChange,
    getHandInfo,
    boardSectionRef,
    isDraggingPiece,
}: MobileLayoutProps): ReactElement {
    // モバイル時のセルサイズを計算
    const mode = gameMode === "playing" ? "playing" : "reviewing";
    const cellSize = useMobileCellSize(mode);

    // 設定BottomSheetの状態
    const [isSettingsOpen, setIsSettingsOpen] = useState(false);

    const topHand = getHandInfo("top");
    const bottomHand = getHandInfo("bottom");

    return (
        <div
            className="flex flex-col items-center w-full px-2"
            style={{ "--shogi-cell-size": `${cellSize}px` } as React.CSSProperties}
        >
            {/* ステータス行 */}
            <div className="flex items-center justify-between w-full py-2 px-2">
                <output className={`${TEXT_CLASSES.moveCount} whitespace-nowrap`}>
                    {moves.length === 0 ? "開始局面" : `${moves.length}手目`}
                </output>

                <output className={`${TEXT_CLASSES.mutedSecondary} whitespace-nowrap`}>
                    手番:{" "}
                    <span
                        className={`font-semibold text-[15px] ${
                            position.turn === "sente" ? "text-wafuu-shu" : "text-wafuu-ai"
                        }`}
                    >
                        {position.turn === "sente" ? "先手" : "後手"}
                    </span>
                </output>

                <button
                    type="button"
                    onClick={onFlipBoard}
                    className="flex items-center justify-center w-8 h-8 rounded-md hover:bg-muted"
                    title="盤面を反転"
                >
                    🔄
                </button>
            </div>

            {/* クロック表示（対局中のみ） */}
            {isMatchRunning && <MobileClockDisplay clocks={clocks} sides={sides} />}

            {/* 盤面セクション */}
            <div
                ref={boardSectionRef}
                className={`relative ${isDraggingPiece ? "touch-none" : ""}`}
            >
                {/* 上側の持ち駒 */}
                <div data-zone={`hand-${topHand.owner}`} className="mb-1">
                    <HandPiecesDisplay
                        owner={topHand.owner}
                        hand={topHand.hand}
                        selectedPiece={selection?.kind === "hand" ? selection.piece : null}
                        isActive={topHand.isActive}
                        onHandSelect={onHandSelect}
                        onPiecePointerDown={isEditMode ? onHandPiecePointerDown : undefined}
                        isEditMode={isEditMode && !isMatchRunning}
                        onIncrement={
                            onIncrementHand
                                ? (piece) => onIncrementHand(topHand.owner, piece)
                                : undefined
                        }
                        onDecrement={
                            onDecrementHand
                                ? (piece) => onDecrementHand(topHand.owner, piece)
                                : undefined
                        }
                        flipBoard={flipBoard}
                        compact
                    />
                </div>

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
                    onSelect={onSquareSelect}
                    onPromotionChoice={onPromotionChoice}
                    flipBoard={flipBoard}
                    onPiecePointerDown={isEditMode ? onPiecePointerDown : undefined}
                    onPieceTogglePromote={isEditMode ? onPieceTogglePromote : undefined}
                    squareNotation={displaySettings.squareNotation}
                    showBoardLabels={displaySettings.showBoardLabels}
                />

                {candidateNote ? (
                    <div className={`${TEXT_CLASSES.mutedSecondary} text-center mt-1`}>
                        {candidateNote}
                    </div>
                ) : null}

                {/* 下側の持ち駒 */}
                <div data-zone={`hand-${bottomHand.owner}`} className="mt-1">
                    <HandPiecesDisplay
                        owner={bottomHand.owner}
                        hand={bottomHand.hand}
                        selectedPiece={selection?.kind === "hand" ? selection.piece : null}
                        isActive={bottomHand.isActive}
                        onHandSelect={onHandSelect}
                        onPiecePointerDown={isEditMode ? onHandPiecePointerDown : undefined}
                        isEditMode={isEditMode && !isMatchRunning}
                        onIncrement={
                            onIncrementHand
                                ? (piece) => onIncrementHand(bottomHand.owner, piece)
                                : undefined
                        }
                        onDecrement={
                            onDecrementHand
                                ? (piece) => onDecrementHand(bottomHand.owner, piece)
                                : undefined
                        }
                        flipBoard={flipBoard}
                        compact
                    />
                </div>
            </div>

            {/* モード別UI */}
            {gameMode === "playing" ? (
                /* 対局モード: 1行棋譜 + 停止ボタン */
                <div className="w-full mt-2 space-y-2">
                    {kifMoves && kifMoves.length > 0 && (
                        <MobileKifuBar
                            moves={kifMoves}
                            currentPly={currentPly}
                            onPlySelect={onPlySelect}
                        />
                    )}
                    {onStop && (
                        <div className="flex justify-center py-2">
                            <button
                                type="button"
                                onClick={onStop}
                                className="px-8 py-3 bg-destructive text-destructive-foreground rounded-lg font-medium shadow-md active:scale-95 transition-transform"
                            >
                                停止
                            </button>
                        </div>
                    )}
                </div>
            ) : isReviewMode ? (
                /* 検討モード: 評価値バー + ナビゲーション + 操作ボタン */
                <div className="w-full mt-2 space-y-1">
                    {/* 評価値バー */}
                    <EvalBar evalCp={evalCp} evalMate={evalMate} />

                    {/* ナビゲーションボタン */}
                    {onBack && onForward && onToStart && onToEnd && (
                        <MobileNavigation
                            currentPly={currentPly}
                            totalPly={totalPly}
                            onBack={onBack}
                            onForward={onForward}
                            onToStart={onToStart}
                            onToEnd={onToEnd}
                        />
                    )}

                    {/* 簡易棋譜表示 */}
                    {kifMoves && kifMoves.length > 0 && (
                        <MobileKifuBar
                            moves={kifMoves}
                            currentPly={currentPly}
                            onPlySelect={onPlySelect}
                        />
                    )}

                    {/* 操作ボタン */}
                    <div className="flex justify-center gap-3 py-2">
                        {onResetToStartpos && (
                            <button
                                type="button"
                                onClick={onResetToStartpos}
                                className="px-4 py-2 border border-border rounded-lg text-sm font-medium hover:bg-muted active:scale-95 transition-all"
                            >
                                平手に戻す
                            </button>
                        )}
                        {onStartReview && (
                            <button
                                type="button"
                                onClick={onStartReview}
                                className="px-4 py-2 bg-primary text-primary-foreground rounded-lg text-sm font-medium shadow-md active:scale-95 transition-all"
                            >
                                対局開始
                            </button>
                        )}
                    </div>
                </div>
            ) : (
                /* 編集モード: 操作ボタンは shogi-match.tsx 側で BottomSheet として表示 */
                <div className="w-full mt-2 text-center text-sm text-muted-foreground">
                    盤面をタップして編集
                </div>
            )}

            {/* FAB: 設定ボタン（右下固定） */}
            <button
                type="button"
                onClick={() => setIsSettingsOpen(true)}
                className="fixed bottom-4 right-4 w-14 h-14 rounded-full bg-primary text-primary-foreground shadow-lg flex items-center justify-center text-2xl active:scale-95 transition-transform z-40"
                aria-label="対局設定を開く"
            >
                ⚙️
            </button>

            {/* 設定BottomSheet */}
            <BottomSheet
                isOpen={isSettingsOpen}
                onClose={() => setIsSettingsOpen(false)}
                title="設定"
                height="auto"
            >
                <MobileSettingsSheet
                    sides={sides}
                    onSidesChange={onSidesChange}
                    timeSettings={timeSettings}
                    onTimeSettingsChange={onTimeSettingsChange}
                    currentTurn={position.turn}
                    onTurnChange={onTurnChange}
                    uiEngineOptions={uiEngineOptions}
                    settingsLocked={settingsLocked}
                    isMatchRunning={isMatchRunning}
                    onStartMatch={
                        onStartReview
                            ? () => {
                                  onStartReview();
                                  setIsSettingsOpen(false);
                              }
                            : undefined
                    }
                    onStopMatch={
                        onStop
                            ? () => {
                                  onStop();
                                  setIsSettingsOpen(false);
                              }
                            : undefined
                    }
                    displaySettings={displaySettingsFull}
                    onDisplaySettingsChange={onDisplaySettingsChange}
                />
            </BottomSheet>
        </div>
    );
}
