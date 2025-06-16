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
                // レスポンシブサイズと文字サイズ
                "w-8 h-8 sm:w-10 sm:h-10 lg:w-14 lg:h-14 flex items-center justify-center font-bold rounded transition-all duration-200",
                "text-sm sm:text-lg lg:text-2xl",
                piece.owner === "black" ? "text-black" : "text-red-600 rotate-180",
                isPromoted && "bg-yellow-100 border border-yellow-300",
                "select-none pointer-events-none", // 駒自体はクリック不可、親のボタンがハンドル
            )}
        >
            {display}
        </div>
    );
}
