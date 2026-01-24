import type { LastMove, PieceType, Player, PositionState, Square } from "@shogi/app-core";
import type { ReactElement, RefObject } from "react";
import { memo } from "react";
import type { ShogiBoardCell } from "../../shogi-board";
import { ShogiBoard } from "../../shogi-board";
import { useMobileCellSize } from "../hooks/useMobileCellSize";
import type { DisplaySettings, PassRightsSettings, PromotionSelection } from "../types";
import { HandPiecesDisplay } from "./HandPiecesDisplay";
import { PassRightsDisplay } from "./PassRightsDisplay";

type Selection = { kind: "square"; square: string } | { kind: "hand"; piece: PieceType };

type HandInfo = {
    owner: Player;
    hand: PositionState["hands"]["sente"] | PositionState["hands"]["gote"];
    isActive: boolean;
    isAI: boolean;
};

interface MobileBoardSectionProps {
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
    // 親で事前計算: isEditMode && !isMatchRunning
    isEditModeActive: boolean;
    /** 対局中かどうか（持ち駒表示の制御に使用） */
    isMatchRunning: boolean;
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

    // === パス権表示（オプション） ===
    /** パス権設定 */
    passRightsSettings?: PassRightsSettings;
    /** 現在のパス権状態 */
    passRights?: { sente: number; gote: number };
    /** 現在の手番 */
    turn?: Player;
}

/**
 * モバイル用盤面セクション
 * React.memoでラップし、対局開始時の再レンダリングを防止
 * useMobileCellSize()を内部で呼び、CSS変数を設定
 */
export const MobileBoardSection = memo(function MobileBoardSection({
    grid,
    flipBoard,
    lastMove,
    selection,
    promotionSelection,
    displaySettings,
    isEditModeActive,
    isMatchRunning,
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
    passRightsSettings,
    passRights,
    turn,
}: MobileBoardSectionProps): ReactElement {
    // セルサイズはこのコンポーネント内で管理（画面幅のみから計算）
    const cellSize = useMobileCellSize();

    // 盤面の幅を計算（9セル + 盤面装飾）
    // ShogiBoard: border (1px×2=2) + 段ラベル左右 (px-0.5×2 + 文字幅) × 2 ≈ 30px + border-l (1px)
    // 余裕を持たせて 20px に設定（p-2 削除、マージン縮小後）
    const boardWidth = cellSize * 9 + 20;

    return (
        <div
            ref={boardSectionRef}
            className={`relative mx-auto flex flex-col gap-1 ${isDraggingPiece ? "touch-none" : ""}`}
            style={
                {
                    "--shogi-cell-size": `${cellSize}px`,
                    width: `${boardWidth}px`,
                } as React.CSSProperties
            }
        >
            {/* 上側の持ち駒 */}
            <div data-zone={`hand-${topHand.owner}`}>
                <HandPiecesDisplay
                    owner={topHand.owner}
                    hand={topHand.hand}
                    selectedPiece={selection?.kind === "hand" ? selection.piece : null}
                    isActive={topHand.isActive}
                    onHandSelect={onHandSelect}
                    onPiecePointerDown={isEditModeActive ? onHandPiecePointerDown : undefined}
                    isEditMode={isEditModeActive}
                    isMatchRunning={isMatchRunning}
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
                    size={isEditModeActive ? "compact" : "medium"}
                    isAI={topHand.isAI}
                />
                {/* パス権表示（上側プレイヤー） */}
                {passRightsSettings?.enabled &&
                    passRightsSettings.initialCount > 0 &&
                    passRights && (
                        <div className="flex justify-end mt-0.5">
                            <PassRightsDisplay
                                remaining={passRights[topHand.owner]}
                                max={passRightsSettings.initialCount}
                                isActive={turn === topHand.owner}
                                compact
                            />
                        </div>
                    )}
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
                isDraggable={isEditModeActive}
                squareNotation={displaySettings.squareNotation}
                showBoardLabels={displaySettings.showBoardLabels}
            />

            {candidateNote ? (
                <div className="text-xs text-muted-foreground text-center">{candidateNote}</div>
            ) : null}

            {/* 下側の持ち駒 */}
            <div data-zone={`hand-${bottomHand.owner}`}>
                <HandPiecesDisplay
                    owner={bottomHand.owner}
                    hand={bottomHand.hand}
                    selectedPiece={selection?.kind === "hand" ? selection.piece : null}
                    isActive={bottomHand.isActive}
                    onHandSelect={onHandSelect}
                    onPiecePointerDown={isEditModeActive ? onHandPiecePointerDown : undefined}
                    isEditMode={isEditModeActive}
                    isMatchRunning={isMatchRunning}
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
                    size={isEditModeActive ? "compact" : "medium"}
                    isAI={bottomHand.isAI}
                />
                {/* パス権表示（下側プレイヤー） */}
                {passRightsSettings?.enabled &&
                    passRightsSettings.initialCount > 0 &&
                    passRights && (
                        <div className="flex justify-start mt-0.5">
                            <PassRightsDisplay
                                remaining={passRights[bottomHand.owner]}
                                max={passRightsSettings.initialCount}
                                isActive={turn === bottomHand.owner}
                                compact
                            />
                        </div>
                    )}
            </div>
        </div>
    );
});
