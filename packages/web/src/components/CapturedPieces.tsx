import { cn } from "@/lib/utils";
import type { Hands, Player } from "shogi-core";

interface CapturedPiecesProps {
    hands: Hands;
    player: Player;
}

const pieceOrder = ["飛", "角", "金", "銀", "桂", "香", "歩"] as const;

export function CapturedPieces({ hands, player }: CapturedPiecesProps) {
    const playerHands = hands[player];

    return (
        <div className={cn("p-4 bg-gray-50 rounded-lg", player === "white" && "rotate-180")}>
            <h3
                className={cn(
                    "text-lg font-bold mb-2",
                    player === "black" ? "text-black" : "text-red-600",
                )}
            >
                {player === "black" ? "先手" : "後手"}の持ち駒
            </h3>
            <div className="flex flex-wrap gap-2">
                {pieceOrder.map((pieceType) => {
                    const count = playerHands[pieceType];
                    if (count === 0) return null;

                    return (
                        <div key={pieceType} className="flex items-center">
                            <span
                                className={cn(
                                    "text-2xl font-bold",
                                    player === "black" ? "text-black" : "text-red-600",
                                )}
                            >
                                {pieceType}
                            </span>
                            {count > 1 && <span className="text-sm ml-1">×{count}</span>}
                        </div>
                    );
                })}
                {Object.values(playerHands).every((count) => count === 0) && (
                    <span className="text-gray-500">なし</span>
                )}
            </div>
        </div>
    );
}
