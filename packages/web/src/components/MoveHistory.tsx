import { cn } from "@/lib/utils";
import type { Move } from "shogi-core";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";

interface MoveHistoryProps {
    moveHistory: Move[];
    historyCursor: number;
    canUndo: boolean;
    canRedo: boolean;
    onUndo: () => void;
    onRedo: () => void;
    onGoToMove: (moveIndex: number) => void;
}

// 手を日本語記法に変換する関数
function formatMove(move: Move, moveNumber: number): string {
    const isBlack = move.piece.owner === "black";
    const prefix = isBlack ? "☗" : "☖";

    // 漢数字変換（段の表示用）
    const kanjiNumbers = ["", "一", "二", "三", "四", "五", "六", "七", "八", "九"];

    if (move.type === "drop") {
        return `${prefix}${moveNumber}. ${
            move.piece.type === "pawn"
                ? "歩"
                : move.piece.type === "lance"
                  ? "香"
                  : move.piece.type === "knight"
                    ? "桂"
                    : move.piece.type === "silver"
                      ? "銀"
                      : move.piece.type === "gold"
                        ? "金"
                        : move.piece.type === "bishop"
                          ? "角"
                          : move.piece.type === "rook"
                            ? "飛"
                            : move.piece.type
        }打${move.to.column}${kanjiNumbers[move.to.row]}`;
    }

    const pieceChar =
        move.piece.type === "pawn"
            ? "歩"
            : move.piece.type === "lance"
              ? "香"
              : move.piece.type === "knight"
                ? "桂"
                : move.piece.type === "silver"
                  ? "銀"
                  : move.piece.type === "gold"
                    ? "金"
                    : move.piece.type === "bishop"
                      ? "角"
                      : move.piece.type === "rook"
                        ? "飛"
                        : move.piece.type === "king"
                          ? "王"
                          : move.piece.type === "gyoku"
                            ? "玉"
                            : move.piece.type;

    const promotion = move.promote ? "成" : "";
    const capture = move.captured ? "x" : "";

    return `${prefix}${moveNumber}. ${pieceChar}${capture}${move.to.column}${kanjiNumbers[move.to.row]}${promotion}`;
}

export function MoveHistory({
    moveHistory,
    historyCursor,
    canUndo,
    canRedo,
    onUndo,
    onRedo,
    onGoToMove,
}: MoveHistoryProps) {
    return (
        <Card className="w-80 sm:w-96">
            <CardHeader className="pb-2 sm:pb-3">
                <CardTitle className="text-base sm:text-lg font-bold">棋譜</CardTitle>
                <div className="flex gap-1 sm:gap-2">
                    <Button
                        onClick={onUndo}
                        disabled={!canUndo}
                        size="sm"
                        variant="outline"
                        className="flex-1 text-xs sm:text-sm touch-manipulation"
                        title="戻る (Ctrl+Z)"
                    >
                        ← 戻る
                    </Button>
                    <Button
                        onClick={onRedo}
                        disabled={!canRedo}
                        size="sm"
                        variant="outline"
                        className="flex-1 text-xs sm:text-sm touch-manipulation"
                        title="進む (Ctrl+Y)"
                    >
                        進む →
                    </Button>
                </div>
            </CardHeader>
            <CardContent className="pt-0">
                <div className="max-h-48 sm:max-h-64 overflow-y-auto space-y-1">
                    {/* 初期状態 */}
                    <button
                        type="button"
                        onClick={() => onGoToMove(-1)}
                        className={cn(
                            "w-full text-left px-2 py-1 rounded text-xs sm:text-sm transition-colors touch-manipulation",
                            historyCursor === -1
                                ? "bg-blue-100 text-blue-900 font-medium"
                                : "hover:bg-gray-100",
                        )}
                    >
                        開始局面
                    </button>

                    {/* 手順リスト */}
                    {moveHistory.map((move, index) => {
                        const moveNumber = Math.floor(index / 2) + 1;
                        const isCurrentMove = historyCursor === index;
                        const moveKey = `${index}-${move.type}-${move.to.row}${move.to.column}`;

                        return (
                            <button
                                key={moveKey}
                                type="button"
                                onClick={() => onGoToMove(index)}
                                className={cn(
                                    "w-full text-left px-2 py-1 rounded text-xs sm:text-sm transition-colors touch-manipulation",
                                    isCurrentMove
                                        ? "bg-blue-100 text-blue-900 font-medium"
                                        : "hover:bg-gray-100",
                                )}
                            >
                                {formatMove(move, moveNumber)}
                            </button>
                        );
                    })}

                    {moveHistory.length === 0 && (
                        <p className="text-xs sm:text-sm text-gray-500 italic">
                            まだ手が指されていません
                        </p>
                    )}
                </div>

                {/* キーボードショートカットのヒント */}
                <div className="mt-2 pt-2 border-t border-gray-200">
                    <p className="text-xs text-gray-500 text-center">
                        💡 <span className="font-mono">Ctrl+Z</span> 戻る /{" "}
                        <span className="font-mono">Ctrl+Y</span> 進む
                    </p>
                </div>
            </CardContent>
        </Card>
    );
}
