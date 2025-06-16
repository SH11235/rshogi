import { cn } from "@/lib/utils";
import type { Board as BoardType, Square } from "shogi-core";
import { Piece } from "./Piece";

interface BoardProps {
    board: BoardType;
    selectedSquare: Square | null;
    validMoves: Square[];
    validDropSquares: Square[];
    onSquareClick: (square: Square) => void;
}

export function Board({
    board,
    selectedSquare,
    validMoves,
    validDropSquares,
    onSquareClick,
}: BoardProps) {
    const renderSquare = (row: number, col: number) => {
        const square: Square = {
            row: row as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
            column: col as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
        };
        const squareKey = `${row}${col}` as const;
        const piece = board[squareKey as keyof BoardType];
        const isSelected = selectedSquare?.row === row && selectedSquare?.column === col;
        const isValidMove = validMoves.some((m) => m.row === row && m.column === col);
        const isValidDrop = validDropSquares.some((m) => m.row === row && m.column === col);

        return (
            <button
                type="button"
                key={squareKey}
                className={cn(
                    // レスポンシブサイズ: モバイルで小さく、デスクトップで大きく
                    "w-10 h-10 sm:w-12 sm:h-12 lg:w-16 lg:h-16 border border-gray-800 flex items-center justify-center cursor-pointer transition-all duration-200",
                    // タッチ操作の改善
                    "touch-manipulation active:scale-95",
                    "hover:bg-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-offset-1",
                    // 選択中の駒のハイライト
                    isSelected && "bg-blue-200 hover:bg-blue-300 ring-2 ring-blue-400",
                    // 有効な移動先のハイライト
                    isValidMove &&
                        !isSelected &&
                        "bg-green-100 hover:bg-green-200 ring-1 ring-green-300",
                    // 有効なドロップ先のハイライト
                    isValidDrop &&
                        !isSelected &&
                        !isValidMove &&
                        "bg-purple-100 hover:bg-purple-200 ring-1 ring-purple-300",
                    // 駒がある場合のホバー効果
                    piece && !isSelected && !isValidMove && !isValidDrop && "hover:bg-yellow-50",
                )}
                onClick={() => onSquareClick(square)}
                onTouchStart={(e) => {
                    // タッチ開始時のフィードバック（iOS Safariの遅延を軽減）
                    e.currentTarget.style.backgroundColor = "rgba(0, 0, 0, 0.1)";
                }}
                onTouchEnd={(e) => {
                    // タッチ終了時にスタイルをリセット
                    setTimeout(() => {
                        e.currentTarget.style.backgroundColor = "";
                    }, 100);
                }}
                aria-label={`Square ${row}-${col}${piece ? ` with ${piece.type} piece` : ""}`}
            >
                {piece && <Piece piece={piece} />}
                {/* 有効な移動先にドットを表示（レスポンシブサイズ） */}
                {isValidMove && !piece && (
                    <div className="w-2 h-2 sm:w-3 sm:h-3 bg-green-500 rounded-full opacity-60" />
                )}
                {/* 有効なドロップ先に四角を表示（レスポンシブサイズ） */}
                {isValidDrop && !piece && (
                    <div className="w-2 h-2 sm:w-3 sm:h-3 lg:w-4 lg:h-4 bg-purple-500 opacity-60" />
                )}
            </button>
        );
    };

    return (
        <div className="inline-block border-2 border-gray-900 bg-white">
            <div className="grid grid-cols-9 gap-0">
                {Array.from({ length: 9 }, (_, rowIndex) =>
                    Array.from({ length: 9 }, (_, colIndex) =>
                        renderSquare(rowIndex + 1, 9 - colIndex),
                    ),
                )}
            </div>
            {/* 座標表示（レスポンシブサイズ） */}
            <div className="flex justify-around mt-1 text-xs sm:text-sm font-medium">
                {Array.from({ length: 9 }, (_, i) => (
                    <div key={`col-${9 - i}`} className="w-10 sm:w-12 lg:w-16 text-center">
                        {9 - i}
                    </div>
                ))}
            </div>
        </div>
    );
}
