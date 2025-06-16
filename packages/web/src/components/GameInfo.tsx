import { cn } from "@/lib/utils";
import { useState } from "react";
import type { GameStatus, Move, Player } from "shogi-core";
import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
    AlertDialogTrigger,
} from "./ui/alert-dialog";

interface GameInfoProps {
    currentPlayer: Player;
    gameStatus: GameStatus;
    moveHistory: Move[];
    onReset: () => void;
}

export function GameInfo({ currentPlayer, gameStatus, moveHistory, onReset }: GameInfoProps) {
    const moveCount = moveHistory.length;
    const turn = Math.floor(moveCount / 2) + 1;
    const [isResetDialogOpen, setIsResetDialogOpen] = useState(false);

    const getStatusMessage = () => {
        switch (gameStatus) {
            case "black_win":
                return "先手の勝ち！";
            case "white_win":
                return "後手の勝ち！";
            case "checkmate":
                return "詰み";
            case "check":
                return `王手！ - ${currentPlayer === "black" ? "先手番" : "後手番"}`;
            case "draw":
                return "引き分け";
            case "sennichite":
                return "千日手";
            case "perpetual_check":
                return "連続王手の千日手";
            case "timeout":
                return "時間切れ";
            case "resigned":
                return "投了";
            default:
                return currentPlayer === "black" ? "先手番" : "後手番";
        }
    };

    const getStatusColor = () => {
        switch (gameStatus) {
            case "black_win":
            case "white_win":
                return "text-green-600";
            case "checkmate":
            case "check":
                return "text-red-600";
            case "draw":
            case "sennichite":
            case "perpetual_check":
                return "text-yellow-600";
            case "timeout":
            case "resigned":
                return "text-gray-600";
            default:
                return currentPlayer === "black" ? "text-black" : "text-red-600";
        }
    };

    const isGameOver = !["playing", "check"].includes(gameStatus);
    const hasMovesPlayed = moveCount > 0;

    const handleResetClick = () => {
        if (hasMovesPlayed && !isGameOver) {
            setIsResetDialogOpen(true);
        } else {
            onReset();
        }
    };

    const handleConfirmReset = () => {
        onReset();
        setIsResetDialogOpen(false);
    };

    return (
        <div className="p-3 sm:p-6 bg-white rounded-lg shadow-md max-w-xs sm:max-w-md mx-auto">
            {/* ゲーム状態表示 */}
            <div className="mb-3 sm:mb-4 text-center">
                <h2
                    className={cn(
                        "text-lg sm:text-xl lg:text-2xl font-bold mb-2",
                        getStatusColor(),
                    )}
                >
                    {getStatusMessage()}
                </h2>

                {/* 手数表示 */}
                <div className="flex justify-center gap-2 sm:gap-4 text-xs sm:text-sm text-gray-600 mb-2 sm:mb-3">
                    <span>第{turn}手</span>
                    <span>•</span>
                    <span>総手数: {moveCount}</span>
                </div>

                {/* 追加の状態情報 */}
                {gameStatus === "check" && (
                    <div className="text-red-500 text-xs sm:text-sm font-medium bg-red-50 px-2 sm:px-3 py-1 rounded-full">
                        🔥 王手がかかっています
                    </div>
                )}

                {isGameOver && (
                    <div className="text-gray-500 text-xs sm:text-sm bg-gray-50 px-2 sm:px-3 py-1 rounded-full">
                        ゲーム終了
                    </div>
                )}
            </div>

            {/* 操作ボタン */}
            <div className="flex gap-2 justify-center">
                <AlertDialog open={isResetDialogOpen} onOpenChange={setIsResetDialogOpen}>
                    <AlertDialogTrigger asChild>
                        <button
                            type="button"
                            onClick={handleResetClick}
                            className={cn(
                                "px-4 sm:px-6 py-2 rounded-lg font-medium transition-colors text-sm sm:text-base",
                                "touch-manipulation active:scale-95",
                                isGameOver
                                    ? "bg-green-500 text-white hover:bg-green-600 focus:ring-green-500"
                                    : "bg-blue-500 text-white hover:bg-blue-600 focus:ring-blue-500",
                                "focus:outline-none focus:ring-2 focus:ring-offset-2",
                            )}
                        >
                            {isGameOver ? "新しいゲーム" : "リセット"}
                        </button>
                    </AlertDialogTrigger>
                    <AlertDialogContent>
                        <AlertDialogHeader>
                            <AlertDialogTitle>ゲームをリセットしますか？</AlertDialogTitle>
                            <AlertDialogDescription>
                                現在の対局がリセットされ、すべての手順が失われます。この操作は元に戻せません。
                            </AlertDialogDescription>
                        </AlertDialogHeader>
                        <AlertDialogFooter>
                            <AlertDialogCancel>キャンセル</AlertDialogCancel>
                            <AlertDialogAction onClick={handleConfirmReset}>
                                リセットする
                            </AlertDialogAction>
                        </AlertDialogFooter>
                    </AlertDialogContent>
                </AlertDialog>
            </div>
        </div>
    );
}
