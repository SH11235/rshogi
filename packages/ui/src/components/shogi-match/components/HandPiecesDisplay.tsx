import type { PieceType, Player, PositionState } from "@shogi/app-core";
import type { ReactElement } from "react";
import { PIECE_LABELS } from "../utils/constants";

const HAND_ORDER: PieceType[] = ["R", "B", "G", "S", "N", "L", "P"];

export interface HandPiecesDisplayProps {
    /** 持ち駒を持つプレイヤー */
    owner: Player;
    /** 持ち駒の状態 */
    hand: PositionState["hands"][Player];
    /** 選択中の持ち駒 */
    selectedPiece: PieceType | null;
    /** クリック可能かどうか */
    isActive: boolean;
    /** 持ち駒クリック時のコールバック */
    onHandSelect: (piece: PieceType) => void;
}

export function HandPiecesDisplay({
    owner,
    hand,
    selectedPiece,
    isActive,
    onHandSelect,
}: HandPiecesDisplayProps): ReactElement {
    return (
        <div style={{ display: "flex", flexWrap: "wrap", gap: "6px" }}>
            {HAND_ORDER.map((piece) => {
                const count = hand[piece] ?? 0;
                const selected = selectedPiece === piece;
                return (
                    <button
                        key={`${owner}-${piece}`}
                        type="button"
                        onClick={() => onHandSelect(piece)}
                        disabled={count <= 0 || !isActive}
                        style={{
                            minWidth: "52px",
                            padding: "6px 10px",
                            borderRadius: "10px",
                            border: selected
                                ? "2px solid hsl(var(--primary, 15 86% 55%))"
                                : "1px solid hsl(var(--border, 0 0% 86%))",
                            background:
                                count > 0 ? "hsl(var(--secondary, 210 40% 96%))" : "transparent",
                            color: "hsl(var(--foreground, 222 47% 11%))",
                            cursor: count > 0 && isActive ? "pointer" : "not-allowed",
                        }}
                    >
                        {PIECE_LABELS[piece]} × {count}
                    </button>
                );
            })}
        </div>
    );
}
