import { cn } from "@/lib/utils";
import type { GameStatus, Player } from "shogi-core";

interface GameInfoProps {
    currentPlayer: Player;
    gameStatus: GameStatus;
    onReset: () => void;
}

export function GameInfo({ currentPlayer, gameStatus, onReset }: GameInfoProps) {
    const getStatusMessage = () => {
        switch (gameStatus) {
            case "black_win":
                return "先手の勝ち！";
            case "white_win":
                return "後手の勝ち！";
            case "draw":
                return "引き分け";
            default:
                return currentPlayer === "black" ? "先手番" : "後手番";
        }
    };

    const isGameOver = gameStatus !== "playing";

    return (
        <div className="p-4 bg-white rounded-lg shadow-md">
            <div className="mb-4">
                <h2
                    className={cn(
                        "text-2xl font-bold",
                        currentPlayer === "black" ? "text-black" : "text-red-600",
                        isGameOver && "text-gray-600",
                    )}
                >
                    {getStatusMessage()}
                </h2>
            </div>

            <button
                type="button"
                onClick={onReset}
                className={cn(
                    "px-6 py-2 rounded-lg font-medium transition-colors",
                    "bg-blue-500 text-white hover:bg-blue-600",
                    "focus:outline-none focus:ring-2 focus:ring-blue-500",
                )}
            >
                {isGameOver ? "もう一度" : "リセット"}
            </button>
        </div>
    );
}
