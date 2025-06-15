import { cn } from "@/lib/utils";
import { type Piece as PieceType, getPieceName } from "shogi-core";

interface PieceProps {
    piece: PieceType;
}

export function Piece({ piece }: PieceProps) {
    const display = getPieceName(piece, "ja");
    const isPromoted = piece.promoted;

    return (
        <div
            className={cn(
                "w-14 h-14 flex items-center justify-center text-2xl font-bold rounded",
                piece.owner === "black" ? "text-black" : "text-red-600 rotate-180",
                isPromoted && "bg-yellow-100",
            )}
        >
            {display}
        </div>
    );
}
