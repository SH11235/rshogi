import type { ReactElement } from "react";
import { cn } from "@shogi/design-system";

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
    onSelect?: (square: string) => void;
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

export function ShogiBoard({ grid, selectedSquare, lastMove, onSelect }: ShogiBoardProps): ReactElement {
    return (
        <div className="inline-flex flex-col gap-1 rounded-lg border border-border bg-card p-2 shadow-card">
            <div className="grid grid-cols-9 gap-px border border-border bg-border">
                {grid.map((row, rowIndex) =>
                    row.map((cell, columnIndex) => {
                        const isSelected = selectedSquare === cell.id;
                        const isLastMove =
                            cell.id === lastMove?.from ||
                            cell.id === lastMove?.to ||
                            (lastMove?.from === cell.id && lastMove?.to === cell.id);

                        return (
                            <button
                                key={`${rowIndex}-${columnIndex}-${cell.id}`}
                                type="button"
                                onClick={() => onSelect?.(cell.id)}
                                className={cn(
                                    "relative aspect-square min-w-10 bg-background text-sm transition-colors hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                                    isSelected && "ring-2 ring-primary/70",
                                    isLastMove && "bg-primary/10",
                                )}
                            >
                                {cell.piece ? (
                                    <span
                                        className={cn(
                                            "flex h-full w-full items-center justify-center font-semibold tracking-tight",
                                            cell.piece.owner === "gote" ? "-rotate-180" : "",
                                        )}
                                    >
                                        {PIECE_LABELS[cell.piece.type] ?? cell.piece.type}
                                        {cell.piece.promoted && (
                                            <span className="absolute right-1 top-1 text-[10px] text-destructive">成</span>
                                        )}
                                    </span>
                                ) : null}
                                <span className="pointer-events-none absolute bottom-1 right-1 text-[9px] text-muted-foreground/70">
                                    {cell.id}
                                </span>
                            </button>
                        );
                    }),
                )}
            </div>
        </div>
    );
}
