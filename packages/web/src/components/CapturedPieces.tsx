import { cn } from "@/lib/utils";
import type { Hands, PieceType, Player } from "shogi-core";

interface CapturedPiecesProps {
    hands: Hands;
    player: Player;
    currentPlayer: Player;
    selectedDropPiece: { type: PieceType; player: Player } | null;
    onPieceClick?: (pieceType: PieceType, player: Player) => void;
}

const pieceOrder = ["飛", "角", "金", "銀", "桂", "香", "歩"] as const;

export function CapturedPieces({
    hands,
    player,
    currentPlayer,
    selectedDropPiece,
    onPieceClick,
}: CapturedPiecesProps) {
    const playerHands = hands[player];
    const isCurrentPlayerTurn = player === currentPlayer;

    // 日本語の駒名からPieceTypeに変換するマッピング
    const pieceTypeMap: Record<string, PieceType> = {
        飛: "rook",
        角: "bishop",
        金: "gold",
        銀: "silver",
        桂: "knight",
        香: "lance",
        歩: "pawn",
    };

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

                    const isSelected =
                        selectedDropPiece?.type === pieceTypeMap[pieceType] &&
                        selectedDropPiece?.player === player;
                    const canClick = isCurrentPlayerTurn && onPieceClick;

                    return (
                        <div key={pieceType} className="flex items-center">
                            <button
                                type="button"
                                disabled={!canClick}
                                className={cn(
                                    "text-2xl font-bold p-1 rounded transition-all duration-200",
                                    player === "black" ? "text-black" : "text-red-600",
                                    canClick &&
                                        "hover:bg-gray-200 cursor-pointer focus:outline-none focus:ring-2 focus:ring-blue-500",
                                    !canClick && "cursor-default",
                                    isSelected && "bg-blue-200 ring-2 ring-blue-400",
                                )}
                                onClick={() =>
                                    canClick && onPieceClick(pieceTypeMap[pieceType], player)
                                }
                                aria-label={`${pieceType}を選択`}
                            >
                                {pieceType}
                            </button>
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
