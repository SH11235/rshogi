import type { LastMove, PieceType, Player, PositionState, Square } from "@shogi/app-core";
import type { ReactElement, RefObject } from "react";
import { memo } from "react";
import type { ShogiBoardCell } from "../../shogi-board";
import { ShogiBoard } from "../../shogi-board";
import { useMobileCellSize } from "../hooks/useMobileCellSize";
import type { DisplaySettings, GameMode, PromotionSelection } from "../types";
import { HandPiecesDisplay } from "./HandPiecesDisplay";

type Selection = { kind: "square"; square: string } | { kind: "hand"; piece: PieceType };

type HandInfo = {
    owner: Player;
    hand: PositionState["hands"]["sente"] | PositionState["hands"]["gote"];
    isActive: boolean;
};

export interface MobileBoardSectionProps {
    // === レイアウト計算用 ===
    gameMode: GameMode;
    hasKifuMoves: boolean;

    // === 盤面データ（変更頻度: 低） ===
    grid: ShogiBoardCell[][];
    position: PositionState;
    flipBoard: boolean;

    // === ハイライト（変更頻度: 中） ===
    lastMove?: LastMove;
    selection: Selection | null;
    promotionSelection: PromotionSelection | null;

    // === 表示設定（変更頻度: 低） ===
    displaySettings: Pick<
        DisplaySettings,
        "highlightLastMove" | "squareNotation" | "showBoardLabels"
    >;

    // === 編集状態 ===
    // ★ポイント: isMatchRunning を含まない
    // 親で事前計算: isEditMode && !isMatchRunning
    isEditModeActive: boolean;
    editFromSquare: Square | null;
    candidateNote: string | null;

    // === イベントハンドラ（メモ化必須） ===
    onSquareSelect: (square: string, shiftKey?: boolean) => void;
    onPromotionChoice: (promote: boolean) => void;
    onHandSelect: (piece: PieceType) => void;

    // === 編集モード用ハンドラ ===
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

    // === 持ち駒情報（親で事前計算） ===
    topHand: HandInfo;
    bottomHand: HandInfo;

    // === Ref / その他 ===
    boardSectionRef: RefObject<HTMLDivElement | null>;
    isDraggingPiece: boolean;
}

/**
 * モバイル用盤面セクション
 * React.memoでラップし、対局開始時の再レンダリングを防止
 * useMobileCellSize()を内部で呼び、CSS変数を設定
 */
export const MobileBoardSection = memo(function MobileBoardSection({
    gameMode,
    hasKifuMoves,
    grid,
    flipBoard,
    lastMove,
    selection,
    promotionSelection,
    displaySettings,
    isEditModeActive,
    editFromSquare,
    candidateNote,
    onSquareSelect,
    onPromotionChoice,
    onHandSelect,
    onPiecePointerDown,
    onPieceTogglePromote,
    onHandPiecePointerDown,
    onIncrementHand,
    onDecrementHand,
    topHand,
    bottomHand,
    boardSectionRef,
    isDraggingPiece,
}: MobileBoardSectionProps): ReactElement {
    // セルサイズはこのコンポーネント内で管理（画面幅・高さ・モードから計算）
    const cellSize = useMobileCellSize({ gameMode, hasKifuMoves });

    // 盤面の幅を計算（9セル + 盤面装飾）
    // ShogiBoard: p-2 (8px×2=16) + border (1px×2=2) + mx-1 (4px×2=8) + border-l (1px) = 27px
    // 選択リング ring-[3px] が外側にはみ出すため追加余裕が必要
    // 余裕を持たせて40pxに設定
    const boardWidth = cellSize * 9 + 40;

    return (
        <div
            ref={boardSectionRef}
            className={`relative mx-auto ${isDraggingPiece ? "touch-none" : ""}`}
            style={
                {
                    "--shogi-cell-size": `${cellSize}px`,
                    width: `${boardWidth}px`,
                } as React.CSSProperties
            }
        >
            {/* 上側の持ち駒 */}
            <div data-zone={`hand-${topHand.owner}`} className="mb-1">
                <HandPiecesDisplay
                    owner={topHand.owner}
                    hand={topHand.hand}
                    selectedPiece={selection?.kind === "hand" ? selection.piece : null}
                    isActive={topHand.isActive}
                    onHandSelect={onHandSelect}
                    onPiecePointerDown={isEditModeActive ? onHandPiecePointerDown : undefined}
                    isEditMode={isEditModeActive}
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
                    isEditModeActive && editFromSquare
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
                onPiecePointerDown={isEditModeActive ? onPiecePointerDown : undefined}
                onPieceTogglePromote={isEditModeActive ? onPieceTogglePromote : undefined}
                squareNotation={displaySettings.squareNotation}
                showBoardLabels={displaySettings.showBoardLabels}
            />

            {candidateNote ? (
                <div className="text-xs text-muted-foreground text-center mt-1">
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
                    onPiecePointerDown={isEditModeActive ? onHandPiecePointerDown : undefined}
                    isEditMode={isEditModeActive}
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
    );
});
