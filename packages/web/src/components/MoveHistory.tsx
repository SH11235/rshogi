import { cn } from "@/lib/utils";
import { HISTORY_CURSOR, formatMove } from "shogi-core";
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
                        title="戻る"
                    >
                        ← 戻る
                    </Button>
                    <Button
                        onClick={onRedo}
                        disabled={!canRedo}
                        size="sm"
                        variant="outline"
                        className="flex-1 text-xs sm:text-sm touch-manipulation"
                        title="進む"
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
                        onClick={() => onGoToMove(HISTORY_CURSOR.INITIAL_POSITION)}
                        className={cn(
                            "w-full text-left px-2 py-1 rounded text-xs sm:text-sm transition-colors touch-manipulation",
                            historyCursor === HISTORY_CURSOR.INITIAL_POSITION
                                ? "bg-blue-100 text-blue-900 font-medium"
                                : "hover:bg-gray-100",
                        )}
                    >
                        開始局面
                    </button>

                    {/* 手順リスト */}
                    {moveHistory.map((move, index) => {
                        const moveNumber = index + 1;
                        const isCurrentMove =
                            historyCursor === index ||
                            (historyCursor === HISTORY_CURSOR.LATEST_POSITION &&
                                index === moveHistory.length - 1);
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
            </CardContent>
        </Card>
    );
}
