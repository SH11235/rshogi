import { cn } from "@/lib/utils";
import type { GameStatus, Move, Player } from "shogi-core";

interface GameInfoProps {
    currentPlayer: Player;
    gameStatus: GameStatus;
    moveHistory: Move[];
    onReset: () => void;
}

export function GameInfo({ currentPlayer, gameStatus, moveHistory, onReset }: GameInfoProps) {
    const moveCount = moveHistory.length;
    const turn = Math.floor(moveCount / 2) + 1;

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

    return (
        <div className="p-6 bg-white rounded-lg shadow-md max-w-md mx-auto">
            {/* ゲーム状態表示 */}
            <div className="mb-4 text-center">
                <h2 className={cn("text-2xl font-bold mb-2", getStatusColor())}>
                    {getStatusMessage()}
                </h2>

                {/* 手数表示 */}
                <div className="flex justify-center gap-4 text-sm text-gray-600 mb-3">
                    <span>第{turn}手</span>
                    <span>•</span>
                    <span>総手数: {moveCount}</span>
                </div>

                {/* 追加の状態情報 */}
                {gameStatus === "check" && (
                    <div className="text-red-500 text-sm font-medium bg-red-50 px-3 py-1 rounded-full">
                        🔥 王手がかかっています
                    </div>
                )}

                {isGameOver && (
                    <div className="text-gray-500 text-sm bg-gray-50 px-3 py-1 rounded-full">
                        ゲーム終了
                    </div>
                )}
            </div>

            {/* 操作ボタン */}
            <div className="flex gap-2 justify-center">
                <button
                    type="button"
                    onClick={onReset}
                    className={cn(
                        "px-6 py-2 rounded-lg font-medium transition-colors",
                        isGameOver
                            ? "bg-green-500 text-white hover:bg-green-600 focus:ring-green-500"
                            : "bg-blue-500 text-white hover:bg-blue-600 focus:ring-blue-500",
                        "focus:outline-none focus:ring-2 focus:ring-offset-2",
                    )}
                >
                    {isGameOver ? "新しいゲーム" : "リセット"}
                </button>
            </div>
        </div>
    );
}
