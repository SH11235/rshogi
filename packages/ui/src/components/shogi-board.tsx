import { cn } from "@shogi/design-system";
import { type ReactElement, useRef } from "react";

export type ShogiBoardOwner = "sente" | "gote";

export interface ShogiBoardPiece {
    owner: ShogiBoardOwner;
    type: string;
    promoted?: boolean;
}

export interface ShogiBoardCell {
    id: string;
    piece: ShogiBoardPiece | null;
}

export interface ShogiBoardProps {
    grid: ShogiBoardCell[][];
    selectedSquare?: string | null;
    lastMove?: { from?: string | null; to?: string | null };
    promotionSquare?: string | null;
    onSelect?: (square: string, shiftKey?: boolean) => void;
    onPromotionChoice?: (promote: boolean) => void;
    /** 盤面を反転表示するか（後手視点） */
    flipBoard?: boolean;
    /** 駒の PointerDown イベント（DnD 用） */
    onPiecePointerDown?: (
        square: string,
        piece: ShogiBoardPiece,
        event: React.PointerEvent,
    ) => void;
    /** 盤上の成/不成トグル（編集モード用） */
    onPieceTogglePromote?: (
        square: string,
        piece: ShogiBoardPiece,
        event: React.MouseEvent<HTMLButtonElement>,
    ) => void;
}

const PIECE_LABELS: Record<string, string> = {
    K: "玉",
    R: "飛",
    B: "角",
    G: "金",
    S: "銀",
    N: "桂",
    L: "香",
    P: "歩",
};

/**
 * 将棋盤コンポーネント
 */
export function ShogiBoard({
    grid,
    selectedSquare,
    lastMove,
    promotionSquare,
    onSelect,
    onPromotionChoice,
    flipBoard = false,
    onPiecePointerDown,
    onPieceTogglePromote,
}: ShogiBoardProps): ReactElement {
    const lastPointerTypeRef = useRef<"mouse" | "touch" | "pen" | null>(null);

    return (
        <div className="relative inline-block w-full max-w-[560px] rounded-2xl border border-[hsl(var(--shogi-outer-border))] bg-[radial-gradient(circle_at_30%_20%,#f9e7c9,#e1c08d)] p-3 shadow-[0_10px_30px_rgba(0,0,0,0.18)]">
            <div className="pointer-events-none absolute inset-3 rounded-xl border border-white/60 shadow-[inset_0_1px_0_rgba(255,255,255,0.7)]" />
            <div className="grid grid-cols-9 overflow-hidden rounded-xl border-l border-t border-[hsl(var(--shogi-border))]">
                {grid.map((row, rowIndex) =>
                    row.map((cell, columnIndex) => {
                        const isSelected = selectedSquare === cell.id;
                        const isLastMove = cell.id === lastMove?.from || cell.id === lastMove?.to;
                        const isPromotionSquare = promotionSquare === cell.id;

                        const tone =
                            (rowIndex + columnIndex) % 2 === 0
                                ? "bg-[hsl(var(--shogi-cell-light))]"
                                : "bg-[hsl(var(--shogi-cell-dark))]";

                        return (
                            <div
                                key={`${rowIndex}-${columnIndex}-${cell.id}`}
                                className="relative aspect-square min-w-12 border-b border-r border-[hsl(var(--shogi-border))]"
                            >
                                <button
                                    type="button"
                                    data-square={cell.id}
                                    onPointerDown={(e) => {
                                        lastPointerTypeRef.current = e.pointerType;
                                        if (cell.piece && onPiecePointerDown) {
                                            onPiecePointerDown(cell.id, cell.piece, e);
                                        }
                                    }}
                                    onDoubleClick={(e) => {
                                        if (cell.piece && onPieceTogglePromote) {
                                            e.preventDefault();
                                            e.stopPropagation();
                                            onPieceTogglePromote(cell.id, cell.piece, e);
                                        }
                                    }}
                                    onContextMenu={(e) => {
                                        if (!cell.piece || !onPieceTogglePromote) return;
                                        e.preventDefault();
                                        e.stopPropagation();
                                        if (lastPointerTypeRef.current === "touch") return;
                                        onPieceTogglePromote(cell.id, cell.piece, e);
                                    }}
                                    onClick={(e) => onSelect?.(cell.id, e.shiftKey)}
                                    onKeyDown={(e) => {
                                        if (e.key === "Enter" && e.shiftKey) {
                                            // Shift+Enter で即座に成る
                                            e.preventDefault();
                                            onSelect?.(cell.id, true);
                                        } else if (e.key === "Escape") {
                                            // Escape でキャンセル
                                            e.preventDefault();
                                            onSelect?.(cell.id, false);
                                        }
                                    }}
                                    aria-label={
                                        cell.piece
                                            ? `${cell.id} ${cell.piece.owner === "sente" ? "先手" : "後手"}の${PIECE_LABELS[cell.piece.type] ?? cell.piece.type}${cell.piece.promoted ? "成" : ""}。Shift+クリックで成って移動`
                                            : `${cell.id} 空マス`
                                    }
                                    className={cn(
                                        "absolute inset-0 overflow-hidden text-base font-semibold transition-all duration-150 ease-out focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-[hsl(var(--wafuu-shu))]/70 focus-visible:ring-offset-transparent",
                                        "bg-[radial-gradient(circle_at_20%_20%,rgba(255,255,255,0.3),transparent_38%),radial-gradient(circle_at_80%_80%,rgba(255,255,255,0.18),transparent_40%)] shadow-[inset_0_1px_0_rgba(255,255,255,0.5)] hover:ring-2 hover:ring-inset hover:ring-[hsl(var(--shogi-border))]",
                                        tone,
                                        isSelected &&
                                            "ring-[3px] ring-inset ring-[hsl(var(--wafuu-shu))] bg-[hsl(var(--wafuu-kin)/0.2)]",
                                        isLastMove &&
                                            "outline outline-2 outline-[hsl(var(--wafuu-kin))]/80",
                                    )}
                                >
                                    {cell.piece ? (
                                        <span
                                            className={cn(
                                                "relative flex h-full w-full items-center justify-center text-[18px] leading-none tracking-tight text-[#3a2a16]",
                                                flipBoard
                                                    ? cell.piece.owner === "sente" && "-rotate-180"
                                                    : cell.piece.owner === "gote" && "-rotate-180",
                                            )}
                                        >
                                            <span className="rounded-[10px] bg-[#fdf6ec]/90 px-2 py-[6px] shadow-[0_4px_8px_rgba(0,0,0,0.12),inset_0_1px_0_rgba(255,255,255,0.9)]">
                                                {PIECE_LABELS[cell.piece.type] ?? cell.piece.type}
                                            </span>
                                            {cell.piece.promoted && (
                                                <span className="absolute right-1 top-1 rounded-full bg-[hsl(var(--wafuu-shu))] px-1 text-[10px] font-bold text-white shadow-sm">
                                                    成
                                                </span>
                                            )}
                                        </span>
                                    ) : null}
                                    <span className="pointer-events-none absolute left-1 top-1 text-[9px] font-medium text-[#9a7b4a]">
                                        {cell.id}
                                    </span>
                                </button>
                                {isPromotionSquare && onPromotionChoice && (
                                    <div
                                        className="absolute inset-0 z-10 flex flex-col gap-[2px] p-[2px]"
                                        role="dialog"
                                        aria-label="成り選択"
                                        aria-live="assertive"
                                    >
                                        <button
                                            type="button"
                                            onClick={(e) => {
                                                e.stopPropagation();
                                                onPromotionChoice(true);
                                            }}
                                            onKeyDown={(e) => {
                                                if (e.key === "Enter" || e.key === " ") {
                                                    e.preventDefault();
                                                    e.stopPropagation();
                                                    onPromotionChoice(true);
                                                } else if (e.key === "Escape") {
                                                    e.preventDefault();
                                                    e.stopPropagation();
                                                    onPromotionChoice(false);
                                                }
                                            }}
                                            aria-label="成る"
                                            className="flex-1 rounded-t-md bg-gradient-to-b from-[hsl(var(--wafuu-shu))] to-[hsl(var(--wafuu-shu)/0.8)] text-[14px] font-bold text-white shadow-lg transition-all hover:scale-105 hover:shadow-xl active:scale-95 focus:outline-none focus:ring-2 focus:ring-white focus:ring-offset-2"
                                        >
                                            成
                                        </button>
                                        <button
                                            type="button"
                                            onClick={(e) => {
                                                e.stopPropagation();
                                                onPromotionChoice(false);
                                            }}
                                            onKeyDown={(e) => {
                                                if (e.key === "Enter" || e.key === " ") {
                                                    e.preventDefault();
                                                    e.stopPropagation();
                                                    onPromotionChoice(false);
                                                } else if (e.key === "Escape") {
                                                    e.preventDefault();
                                                    e.stopPropagation();
                                                    onPromotionChoice(false);
                                                }
                                            }}
                                            aria-label="成らない"
                                            className="flex-1 rounded-b-md bg-gradient-to-b from-[#4a90e2] to-[#357abd] text-[12px] font-bold text-white shadow-lg transition-all hover:scale-105 hover:shadow-xl active:scale-95 focus:outline-none focus:ring-2 focus:ring-white focus:ring-offset-2"
                                        >
                                            不成
                                        </button>
                                    </div>
                                )}
                            </div>
                        );
                    }),
                )}
            </div>
        </div>
    );
}
