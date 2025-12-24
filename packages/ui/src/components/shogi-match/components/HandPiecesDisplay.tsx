import type { PieceType, Player, PositionState } from "@shogi/app-core";
import type { ReactElement } from "react";
import { PIECE_CAP, PIECE_LABELS } from "../utils/constants";

const HAND_ORDER: PieceType[] = ["R", "B", "G", "S", "N", "L", "P"];

interface HandPiecesDisplayProps {
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
    /** DnD 用 PointerDown ハンドラ（編集モード時） */
    onPiecePointerDown?: (owner: Player, pieceType: PieceType, e: React.PointerEvent) => void;
    /** 編集モードかどうか */
    isEditMode?: boolean;
    /** 持ち駒を増やす（編集モード用） */
    onIncrement?: (piece: PieceType) => void;
    /** 持ち駒を減らす（編集モード用） */
    onDecrement?: (piece: PieceType) => void;
}

export function HandPiecesDisplay({
    owner,
    hand,
    selectedPiece,
    isActive,
    onHandSelect,
    onPiecePointerDown,
    isEditMode = false,
    onIncrement,
    onDecrement,
}: HandPiecesDisplayProps): ReactElement {
    return (
        <div style={{ display: "flex", flexWrap: "wrap", gap: "6px" }}>
            {HAND_ORDER.map((piece) => {
                const count = hand[piece] ?? 0;
                const selected = selectedPiece === piece;
                // 編集モード時は0個でもドラッグ可能（ストックとして機能）
                const canDrag = (count > 0 || isEditMode) && Boolean(onPiecePointerDown);
                const canSelect = count > 0 && isActive;
                const isDisabled = !canDrag && !canSelect && !isEditMode;
                const maxCount = PIECE_CAP[piece];

                return (
                    <div
                        key={`${owner}-${piece}`}
                        style={{
                            display: "flex",
                            alignItems: "center",
                            gap: "2px",
                        }}
                    >
                        {/* 駒ボタン */}
                        <button
                            type="button"
                            onPointerDown={(e) => {
                                if (canDrag && onPiecePointerDown) {
                                    onPiecePointerDown(owner, piece, e);
                                }
                            }}
                            onClick={(e) => {
                                if (!canSelect) {
                                    e.preventDefault();
                                    return;
                                }
                                onHandSelect(piece);
                            }}
                            disabled={isDisabled}
                            style={{
                                minWidth: "52px",
                                padding: "6px 10px",
                                borderRadius: "10px",
                                border: selected
                                    ? "2px solid hsl(var(--primary, 15 86% 55%))"
                                    : "1px solid hsl(var(--border, 0 0% 86%))",
                                background:
                                    count > 0 || isEditMode
                                        ? "hsl(var(--secondary, 210 40% 96%))"
                                        : "transparent",
                                color: "hsl(var(--foreground, 222 47% 11%))",
                                cursor: canDrag || canSelect ? "pointer" : "default",
                            }}
                        >
                            {PIECE_LABELS[piece]} × {count}
                        </button>

                        {/* 編集モード: ±ボタン（縦並び） */}
                        {isEditMode && (
                            <div
                                style={{
                                    display: "flex",
                                    flexDirection: "column",
                                    gap: "1px",
                                }}
                            >
                                <button
                                    type="button"
                                    onClick={() => onIncrement?.(piece)}
                                    disabled={count >= maxCount}
                                    aria-label={`${PIECE_LABELS[piece]}を増やす`}
                                    style={{
                                        width: "20px",
                                        height: "16px",
                                        borderRadius: "4px 4px 0 0",
                                        border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        borderBottom: "none",
                                        background:
                                            count < maxCount
                                                ? "hsl(var(--secondary, 210 40% 96%))"
                                                : "transparent",
                                        color: "hsl(var(--foreground, 222 47% 11%))",
                                        cursor: count < maxCount ? "pointer" : "not-allowed",
                                        fontSize: "12px",
                                        fontWeight: "bold",
                                        display: "flex",
                                        alignItems: "center",
                                        justifyContent: "center",
                                        lineHeight: 1,
                                    }}
                                >
                                    +
                                </button>
                                <button
                                    type="button"
                                    onClick={() => onDecrement?.(piece)}
                                    disabled={count <= 0}
                                    aria-label={`${PIECE_LABELS[piece]}を減らす`}
                                    style={{
                                        width: "20px",
                                        height: "16px",
                                        borderRadius: "0 0 4px 4px",
                                        border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        background:
                                            count > 0
                                                ? "hsl(var(--secondary, 210 40% 96%))"
                                                : "transparent",
                                        color: "hsl(var(--foreground, 222 47% 11%))",
                                        cursor: count > 0 ? "pointer" : "not-allowed",
                                        fontSize: "12px",
                                        fontWeight: "bold",
                                        display: "flex",
                                        alignItems: "center",
                                        justifyContent: "center",
                                        lineHeight: 1,
                                    }}
                                >
                                    −
                                </button>
                            </div>
                        )}
                    </div>
                );
            })}
        </div>
    );
}
