import type { PieceType, Player, PositionState } from "@shogi/app-core";
import { cn } from "@shogi/design-system";
import type { ReactElement } from "react";
import { PIECE_CAP, PIECE_LABELS } from "../utils/constants";

const HAND_ORDER: PieceType[] = ["R", "B", "G", "S", "N", "L", "P"];

/** サイズ設定: compact=編集用, medium=モバイル対局用, normal=PC用 */
type HandPieceSize = "compact" | "medium" | "normal";

const SIZE_CONFIG = {
    compact: {
        text: "text-[11px]",
        padding: "px-1 py-0.5",
        badgeSize: "min-w-[10px] text-[8px]",
        badgePos: "-bottom-0.5 -right-0.5",
        badgePosRotated: "-left-0.5 -top-0.5 rotate-180",
    },
    medium: {
        text: "text-[14px]",
        padding: "px-1.5 py-1",
        badgeSize: "min-w-[12px] text-[9px]",
        badgePos: "-bottom-0.5 -right-0.5",
        badgePosRotated: "-left-0.5 -top-0.5 rotate-180",
    },
    normal: {
        text: "text-[16px]",
        padding: "px-2 py-[5px]",
        badgeSize: "min-w-[14px] text-[11px]",
        badgePos: "-bottom-1 -right-1",
        badgePosRotated: "-left-1 -top-1 rotate-180",
    },
} as const;

/** 盤上の駒と同じスタイルの駒表示 */
function PieceToken({
    pieceType,
    owner,
    count,
    flipBoard = false,
    size = "normal",
}: {
    pieceType: PieceType;
    owner: Player;
    count: number;
    flipBoard?: boolean;
    size?: HandPieceSize;
}): ReactElement {
    // 盤面と同じ回転ロジック: 反転時は先手が逆さ、通常時は後手が逆さ
    const shouldRotate = flipBoard ? owner === "sente" : owner === "gote";
    const config = SIZE_CONFIG[size];

    return (
        <span
            className={cn(
                "relative inline-flex items-center justify-center leading-none tracking-tight text-[#3a2a16]",
                config.text,
                shouldRotate && "-rotate-180",
            )}
        >
            <span
                className={cn(
                    "rounded-[8px] bg-[#fdf6ec]/90 shadow-[0_3px_6px_rgba(0,0,0,0.12),inset_0_1px_0_rgba(255,255,255,0.9)]",
                    config.padding,
                )}
            >
                {PIECE_LABELS[pieceType]}
            </span>
            {/* 個数を添え字として表示（親が回転しても常に右下に表示） */}
            <span
                className={cn(
                    "absolute text-center font-bold leading-none",
                    config.badgeSize,
                    shouldRotate ? config.badgePosRotated : config.badgePos,
                    count > 0
                        ? "text-[hsl(var(--wafuu-sumi))]"
                        : "text-[hsl(var(--muted-foreground))]",
                )}
            >
                {count}
            </span>
        </span>
    );
}

// コンテナ用のサイズ設定
const CONTAINER_SIZE_CONFIG = {
    compact: {
        container: "flex-nowrap gap-0.5 min-h-[24px]",
        marker: "text-sm mr-0.5",
        buttonPadding: "p-0.5",
        gap: "1px",
    },
    medium: {
        container: "flex-nowrap gap-1 min-h-[32px]",
        marker: "text-base mr-0.5",
        buttonPadding: "p-0.5",
        gap: "2px",
    },
    normal: {
        container: "flex-wrap gap-1.5 min-h-[44px]",
        marker: "text-xl",
        buttonPadding: "p-1",
        gap: "2px",
    },
} as const;

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
    /** 盤面反転状態 */
    flipBoard?: boolean;
    /** サイズ: compact=編集用, medium=モバイル対局用, normal=PC用 */
    size?: HandPieceSize;
    /** 固定レイアウト: trueの場合、0個の駒もスペースを確保（対局中のレイアウトシフト防止） */
    fixedLayout?: boolean;
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
    flipBoard = false,
    size = "normal",
    fixedLayout = false,
}: HandPiecesDisplayProps): ReactElement {
    // 先手/後手マーカー（☗=U+2617, ☖=U+2616）
    const ownerMarker = owner === "sente" ? "☗" : "☖";
    // 先手: 朱色、後手: 藍色（wafuuテーマ）
    const markerColorClass = owner === "sente" ? "text-wafuu-shu" : "text-wafuu-ai";
    const containerConfig = CONTAINER_SIZE_CONFIG[size];
    const isCompactLayout = size === "compact" || size === "medium";

    return (
        <div className={cn("flex items-center", containerConfig.container)}>
            {/* 先手/後手マーカー */}
            <span
                className={cn(
                    markerColorClass,
                    "font-bold select-none shrink-0",
                    containerConfig.marker,
                )}
                title={owner === "sente" ? "先手" : "後手"}
            >
                {ownerMarker}
            </span>
            {HAND_ORDER.map((piece) => {
                const count = hand[piece] ?? 0;

                // 対局時は0個の駒を非表示（ただしvisibilityで隠してスペースは確保）
                const isHidden = !isEditMode && count === 0;

                const selected = selectedPiece === piece;
                // 編集モード時は0個でもドラッグ可能（ストックとして機能）
                const canDrag = (count > 0 || isEditMode) && Boolean(onPiecePointerDown);
                const canSelect = count > 0 && isActive;
                const isDisabled = !canDrag && !canSelect && !isEditMode;
                const maxCount = PIECE_CAP[piece];

                // コンパクトレイアウトかつ非編集時は0個の駒は完全に非表示（スペースも確保しない）
                // ただし、fixedLayoutがtrueの場合はスペースを確保（対局中のレイアウトシフト防止）
                // 編集モードでは全ての駒を表示する
                if (isCompactLayout && count === 0 && !isEditMode && !fixedLayout) {
                    return null;
                }

                return (
                    <div
                        key={`${owner}-${piece}`}
                        style={{
                            display: "flex",
                            alignItems: "center",
                            gap: containerConfig.gap,
                            visibility: isHidden && !isCompactLayout ? "hidden" : "visible",
                        }}
                    >
                        {/* 駒ボタン */}
                        <button
                            type="button"
                            onPointerDown={(e) => {
                                if (canDrag && onPiecePointerDown) {
                                    // タッチ時のテキスト選択・長押しメニューを防止
                                    e.preventDefault();
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
                                "relative rounded-lg border-2 transition-all",
                                // タッチ選択・長押しメニュー防止
                                "select-none [-webkit-touch-callout:none]",
                                // 編集モード（ドラッグ可能）時はスクロールも防止
                                canDrag ? "touch-none" : "touch-manipulation",
                                containerConfig.buttonPadding,
                                selected
                                    ? "border-[hsl(var(--wafuu-shu))] bg-[hsl(var(--wafuu-kin)/0.2)]"
                                    : "border-transparent",
                                count > 0 || isEditMode ? "opacity-100" : "opacity-40",
                                (canDrag || canSelect) &&
                                    "cursor-pointer hover:bg-[hsl(var(--wafuu-kin)/0.1)]",
                            )}
                        >
                            <PieceToken
                                pieceType={piece}
                                owner={owner}
                                count={count}
                                flipBoard={flipBoard}
                                size={size}
                            />
                        </button>

                        {/* ±ボタン（縦並び）- 編集モードでなくてもスペースを確保、compact/mediumモード時は編集モードのみ表示 */}
                        {(!isCompactLayout || isEditMode) && (
                            <div
                                style={{
                                    display: "flex",
                                    flexDirection: "column",
                                    gap: "1px",
                                    visibility: isEditMode ? "visible" : "hidden",
                                }}
                            >
                                <button
                                    type="button"
                                    onClick={() => onIncrement?.(piece)}
                                    disabled={!isEditMode || count >= maxCount}
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
                                    disabled={!isEditMode || count <= 0}
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
