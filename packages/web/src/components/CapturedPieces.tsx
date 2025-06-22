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
        <div
            className={cn(
                "p-2 sm:p-4 bg-gray-50 rounded-lg w-full",
                player === "white" && "rotate-180",
            )}
        >
            <h3
                className={cn(
                    "text-sm sm:text-base lg:text-lg font-bold mb-2",
                    player === "black" ? "text-black" : "text-red-600",
                )}
            >
                {player === "black" ? "先手" : "後手"}の持ち駒
            </h3>
            {/* 固定高さを設定してレイアウトシフトを防止 */}
            <div className="min-h-[40px] sm:min-h-[48px] lg:min-h-[56px]">
                <div className="flex flex-wrap gap-1 sm:gap-2">
                    {pieceOrder
                        .filter((pieceType) => playerHands[pieceType] > 0)
                        .map((pieceType) => {
                            const count = playerHands[pieceType];
                            const isSelected =
                                selectedDropPiece?.type === pieceTypeMap[pieceType] &&
                                selectedDropPiece?.player === player;
                            const canClick = isCurrentPlayerTurn && onPieceClick;

                            return (
                                <div key={pieceType} className="flex items-center animate-fadeIn">
                                    <button
                                        type="button"
                                        disabled={!canClick}
                                        className={cn(
                                            // レスポンシブフォントサイズとパディング
                                            "text-lg sm:text-xl lg:text-2xl font-bold p-1 sm:p-2 rounded transition-all duration-200",
                                            // タッチ操作の改善
                                            "touch-manipulation active:scale-95",
                                            player === "black" ? "text-black" : "text-red-600",
                                            canClick &&
                                                "hover:bg-gray-200 cursor-pointer focus:outline-none focus:ring-2 focus:ring-blue-500",
                                            !canClick && "cursor-default",
                                            isSelected && "bg-blue-200 ring-2 ring-blue-400",
                                        )}
                                        onClick={() =>
                                            canClick &&
                                            onPieceClick(pieceTypeMap[pieceType], player)
                                        }
                                        onTouchStart={(e) => {
                                            if (canClick) {
                                                e.currentTarget.style.backgroundColor =
                                                    "rgba(0, 0, 0, 0.1)";
                                            }
                                        }}
                                        onTouchEnd={(e) => {
                                            if (canClick) {
                                                setTimeout(() => {
                                                    e.currentTarget.style.backgroundColor = "";
                                                }, 100);
                                            }
                                        }}
                                        aria-label={`${pieceType}を選択`}
                                    >
                                        {pieceType}
                                    </button>
                                    {count > 1 && <span className="text-sm ml-1">×{count}</span>}
                                </div>
                            );
                        })}
                    {/* 「なし」の表示 */}
                    {Object.values(playerHands).every((count) => count === 0) && (
                        <span className="text-gray-500">なし</span>
                    )}
                </div>
            </div>
        </div>
    );
}
