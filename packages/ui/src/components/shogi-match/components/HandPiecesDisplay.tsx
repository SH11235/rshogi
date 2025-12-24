import { cn } from "@shogi/design-system";
import type { PieceType, Player, PositionState } from "@shogi/app-core";
import type { ReactElement } from "react";
import { PIECE_CAP, PIECE_LABELS } from "../utils/constants";

const HAND_ORDER: PieceType[] = ["R", "B", "G", "S", "N", "L", "P"];

/** 盤上の駒と同じスタイルの駒表示 */
function PieceToken({
    pieceType,
    owner,
    count,
}: {
    pieceType: PieceType;
    owner: Player;
    count: number;
}): ReactElement {
    return (
        <span
            className={cn(
                "relative inline-flex items-center justify-center text-[16px] leading-none tracking-tight text-[#3a2a16]",
                owner === "gote" && "-rotate-180",
            )}
        >
            <span className="rounded-[8px] bg-[#fdf6ec]/90 px-2 py-[5px] shadow-[0_3px_6px_rgba(0,0,0,0.12),inset_0_1px_0_rgba(255,255,255,0.9)]">
                {PIECE_LABELS[pieceType]}
            </span>
            {/* 個数を添え字として表示 */}
            <span
                className={cn(
                    "absolute -bottom-1 -right-1 min-w-[14px] text-center text-[11px] font-bold leading-none",
                    count > 0
                        ? "text-[hsl(var(--wafuu-sumi))]"
                        : "text-[hsl(var(--muted-foreground))]",
                    owner === "gote" && "rotate-180",
                )}
            >
                {count}
            </span>
        </span>
    );
}

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

                // 対局時は0個の駒を非表示
                if (!isEditMode && count === 0) {
                    return null;
                }

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
                            className={cn(
                                "relative rounded-lg border-2 p-1 transition-all",
                                selected
                                    ? "border-[hsl(var(--wafuu-shu))] bg-[hsl(var(--wafuu-kin)/0.2)]"
                                    : "border-transparent",
                                count > 0 || isEditMode ? "opacity-100" : "opacity-40",
                                (canDrag || canSelect) &&
                                    "cursor-pointer hover:bg-[hsl(var(--wafuu-kin)/0.1)]",
                            )}
                        >
                            <PieceToken pieceType={piece} owner={owner} count={count} />
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
                                                ? "hsl(var(--wafuu-washi))"
                                                : "hsl(var(--muted, 210 40% 96%))",
                                        color:
                                            count < maxCount
                                                ? "hsl(var(--wafuu-sumi))"
                                                : "hsl(var(--muted-foreground, 0 0% 70%))",
                                        cursor: count < maxCount ? "pointer" : "not-allowed",
                                        fontSize: "12px",
                                        fontWeight: "bold",
                                        display: "flex",
                                        alignItems: "center",
                                        justifyContent: "center",
                                        lineHeight: 1,
                                        opacity: count < maxCount ? 1 : 0.4,
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
                                                ? "hsl(var(--wafuu-washi))"
                                                : "hsl(var(--muted, 210 40% 96%))",
                                        color:
                                            count > 0
                                                ? "hsl(var(--wafuu-sumi))"
                                                : "hsl(var(--muted-foreground, 0 0% 70%))",
                                        cursor: count > 0 ? "pointer" : "not-allowed",
                                        fontSize: "12px",
                                        fontWeight: "bold",
                                        display: "flex",
                                        alignItems: "center",
                                        justifyContent: "center",
                                        lineHeight: 1,
                                        opacity: count > 0 ? 1 : 0.4,
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
