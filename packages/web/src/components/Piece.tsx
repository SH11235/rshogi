import { cn } from "@/lib/utils";
import { PROMOTED_PIECE_NAMES, type Piece as PieceType, getPieceName } from "shogi-core";

interface PieceProps {
    piece: PieceType;
}

export function Piece({ piece }: PieceProps) {
    const display =
        piece.promoted && PROMOTED_PIECE_NAMES.ja[piece.type]
            ? PROMOTED_PIECE_NAMES.ja[piece.type]
            : getPieceName(piece, "ja");
    const isPromoted = piece.promoted;
    // 成香は縦書き表示が必要
    const needsVerticalText = display === "成香";

    return (
        <div className="relative w-full h-full flex items-center justify-center">
            {/* 五角形（将棋駒型）の背景をSVGで描画 */}
            <svg
                className={cn(
                    "absolute inset-0 w-full h-full",
                    piece.owner === "white" && "rotate-180",
                )}
                viewBox="0 0 100 100"
                preserveAspectRatio="xMidYMid meet"
                aria-hidden="true"
            >
                <polygon
                    points="50,10 80,40 80,90 20,90 20,40"
                    className={cn(
                        "transition-all duration-200",
                        isPromoted ? "fill-[#ffd700]" : "fill-[#f5deb3]",
                        "stroke-gray-800 stroke-1",
                    )}
                />
            </svg>
            {/* 駒の文字 */}
            <div
                className={cn(
                    // レスポンシブサイズと文字サイズ
                    "relative z-10 flex items-center justify-center font-bold transition-all duration-200",
                    needsVerticalText
                        ? "text-xs sm:text-sm lg:text-base"
                        : "text-sm sm:text-lg lg:text-xl",
                    piece.owner === "black" ? "text-black" : "text-red-600",
                    "select-none pointer-events-none", // 駒自体はクリック不可、親のボタンがハンドル
                )}
                style={{
                    // 五角形の重心に合わせて文字位置を調整
                    // 後手は文字も180度回転させ、位置を上に調整
                    transform:
                        piece.owner === "white"
                            ? "rotate(180deg) translateY(8%)" // 回転後の座標系で下に移動（見た目は上）
                            : "translateY(8%)",
                }}
            >
                {needsVerticalText ? (
                    // 成香を縦書きで表示
                    <div className="flex flex-col items-center leading-none">
                        <span>成</span>
                        <span>香</span>
                    </div>
                ) : (
                    display
                )}
            </div>
        </div>
    );
}
