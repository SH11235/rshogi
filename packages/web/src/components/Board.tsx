import { cn } from "@/lib/utils";
import type { Board as BoardType, Square } from "shogi-core";
import { Piece } from "./Piece";

interface BoardProps {
    board: BoardType;
    selectedSquare: Square | null;
    validMoves: Square[];
    onSquareClick: (square: Square) => void;
}

export function Board({ board, selectedSquare, validMoves, onSquareClick }: BoardProps) {
    const renderSquare = (row: number, col: number) => {
        const square: Square = {
            row: row as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
            column: col as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
        };
        const squareKey = `${row}${col}` as const;
        const piece = board[squareKey as keyof BoardType];
        const isSelected = selectedSquare?.row === row && selectedSquare?.column === col;
        const isValidMove = validMoves.some((m) => m.row === row && m.column === col);

        return (
            <button
                type="button"
                key={squareKey}
                className={cn(
                    "w-16 h-16 border border-gray-800 flex items-center justify-center cursor-pointer transition-all",
                    "hover:bg-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500",
                    isSelected && "bg-blue-200 hover:bg-blue-300",
                    isValidMove && "bg-green-100 hover:bg-green-200",
                )}
                onClick={() => onSquareClick(square)}
            >
                {piece && <Piece piece={piece} />}
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
            {/* 座標表示 */}
            <div className="flex justify-around mt-1 text-sm font-medium">
                {Array.from({ length: 9 }, (_, i) => (
                    <div key={`col-${9 - i}`} className="w-16 text-center">
                        {9 - i}
                    </div>
                ))}
            </div>
        </div>
    );
}
