import { cn } from "@shogi/design-system";
import type { ReactElement } from "react";

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

export function ShogiBoard({
    grid,
    selectedSquare,
    lastMove,
    onSelect,
}: ShogiBoardProps): ReactElement {
    return (
        <div className="relative inline-block w-full max-w-[560px] rounded-2xl border border-[#c08a3d] bg-[radial-gradient(circle_at_30%_20%,#f9e7c9,#e1c08d)] p-3 shadow-[0_10px_30px_rgba(0,0,0,0.18)]">
            <div className="pointer-events-none absolute inset-3 rounded-xl border border-white/60 shadow-[inset_0_1px_0_rgba(255,255,255,0.7)]" />
            <div className="grid grid-cols-9 gap-[4px] rounded-xl border border-[#c08a3d]/70 bg-[#c08a3d]/30 p-[6px] backdrop-blur-[1px]">
                {grid.map((row, rowIndex) =>
                    row.map((cell, columnIndex) => {
                        const isSelected = selectedSquare === cell.id;
                        const isLastMove =
                            cell.id === lastMove?.from ||
                            cell.id === lastMove?.to ||
                            (lastMove?.from === cell.id && lastMove?.to === cell.id);

                        const tone =
                            (rowIndex + columnIndex) % 2 === 0 ? "bg-[#f3e1c7]" : "bg-[#ead2ac]";

                        return (
                            <button
                                key={`${rowIndex}-${columnIndex}-${cell.id}`}
                                type="button"
                                onClick={() => onSelect?.(cell.id)}
                                className={cn(
                                    "relative aspect-square min-w-12 overflow-hidden rounded-md border border-[#c7a165] text-base font-semibold transition-all duration-150 ease-out focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-[#f06c3c]/70 focus-visible:ring-offset-transparent",
                                    "bg-[radial-gradient(circle_at_20%_20%,rgba(255,255,255,0.3),transparent_38%),radial-gradient(circle_at_80%_80%,rgba(255,255,255,0.18),transparent_40%)] shadow-[inset_0_1px_0_rgba(255,255,255,0.5),0_6px_12px_rgba(0,0,0,0.15)] hover:-translate-y-[1px]",
                                    tone,
                                    isSelected &&
                                        "ring-2 ring-[hsl(var(--wafuu-shu))]/70 ring-offset-1 shadow-[0_0_12px_hsl(var(--wafuu-kin)/0.4)]",
                                    isLastMove && "outline outline-2 outline-[#f0b03c]/80",
                                )}
                            >
                                {cell.piece ? (
                                    <span
                                        className={cn(
                                            "relative flex h-full w-full items-center justify-center text-[18px] leading-none tracking-tight text-[#3a2a16]",
                                            cell.piece.owner === "gote" ? "-rotate-180" : "",
                                        )}
                                    >
                                        <span className="rounded-[10px] bg-[#fdf6ec]/90 px-2 py-[6px] shadow-[0_4px_8px_rgba(0,0,0,0.12),inset_0_1px_0_rgba(255,255,255,0.9)]">
                                            {PIECE_LABELS[cell.piece.type] ?? cell.piece.type}
                                        </span>
                                        {cell.piece.promoted && (
                                            <span className="absolute right-1 top-1 rounded-full bg-[#f06c3c] px-1 text-[10px] font-bold text-white shadow-sm">
                                                成
                                            </span>
                                        )}
                                    </span>
                                ) : null}
                                <span className="pointer-events-none absolute left-1 top-1 text-[9px] font-medium text-[#9a7b4a]">
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
