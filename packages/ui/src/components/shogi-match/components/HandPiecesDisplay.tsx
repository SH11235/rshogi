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
        piecesContainer: "flex-nowrap gap-0",
        marker: "text-sm w-4",
        buttonPadding: "p-0.5",
    },
    medium: {
        piecesContainer: "flex-nowrap gap-0.5",
        marker: "text-base w-5",
        buttonPadding: "p-0.5",
    },
    normal: {
        piecesContainer: "flex-wrap gap-0.5",
        marker: "text-xl w-7",
        buttonPadding: "p-0.5",
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
    /** 対局中かどうか（対局中のみ持ち駒がない駒を非表示にする） */
    isMatchRunning?: boolean;
    /** 持ち駒を増やす（編集モード用） */
    onIncrement?: (piece: PieceType) => void;
    /** 持ち駒を減らす（編集モード用） */
    onDecrement?: (piece: PieceType) => void;
    /** 盤面反転状態 */
    flipBoard?: boolean;
    /** サイズ: compact=編集用, medium=モバイル対局用, normal=PC用 */
    size?: HandPieceSize;
}

export function HandPiecesDisplay({
    owner,
    hand,
    selectedPiece,
    isActive,
    onHandSelect,
    onPiecePointerDown,
    isEditMode = false,
    isMatchRunning = false,
    onIncrement,
    onDecrement,
    flipBoard = false,
    size = "normal",
}: HandPiecesDisplayProps): ReactElement {
    // 先手/後手マーカー（☗=U+2617, ☖=U+2616）
    const ownerMarker = owner === "sente" ? "☗" : "☖";
    // 先手: 朱色、後手: 藍色（wafuuテーマ）
    const markerColorClass = owner === "sente" ? "text-wafuu-shu" : "text-wafuu-ai";
    const containerConfig = CONTAINER_SIZE_CONFIG[size];
    const isCompactLayout = size === "compact" || size === "medium";

    return (
        <div
            className={cn(
                "flex items-center justify-start w-full rounded-md border border-border/50 bg-muted/30 px-1",
            )}
        >
            {/* 先手/後手マーカー - 固定幅で左端に配置 */}
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
            {/* 持ち駒コンテナ - 駒だけが詰まる */}
            <div className={cn("flex items-center relative", containerConfig.piecesContainer)}>
                {/* 横幅/高さ確保用のダミー要素 */}
                {!isCompactLayout ? (
                    // PC版: 全7種の駒＋±ボタンで横幅を確保
                    <div className="invisible flex items-center gap-0.5" aria-hidden="true">
                        {HAND_ORDER.map((piece) => (
                            <div key={piece} className="flex items-center gap-px">
                                <div
                                    className={cn(
                                        "border-2 border-transparent rounded-lg",
                                        containerConfig.buttonPadding,
                                    )}
                                >
                                    <PieceToken
                                        pieceType={piece}
                                        owner={owner}
                                        count={0}
                                        flipBoard={flipBoard}
                                        size={size}
                                    />
                                </div>
                                {/* ±ボタン分のスペース */}
                                <div className="flex flex-col gap-px">
                                    <div className="h-4 w-5" />
                                    <div className="h-4 w-5" />
                                </div>
                            </div>
                        ))}
                    </div>
                ) : (
                    // モバイル版: 高さ確保用の1つの駒
                    <div className="invisible" aria-hidden="true">
                        <div
                            className={cn(
                                "border-2 border-transparent rounded-lg",
                                containerConfig.buttonPadding,
                            )}
                        >
                            <PieceToken
                                pieceType="P"
                                owner={owner}
                                count={0}
                                flipBoard={flipBoard}
                                size={size}
                            />
                        </div>
                    </div>
                )}

                {/* 実際の駒（PC版は absolute で左寄せ、モバイル版は通常フロー） */}
                <div
                    data-testid="hand-pieces-actual"
                    className={
                        !isCompactLayout
                            ? "absolute left-0 top-0 flex items-center gap-0.5 h-full"
                            : "contents"
                    }
                >
                    {HAND_ORDER.map((piece) => {
                        const count = hand[piece] ?? 0;

                        const selected = selectedPiece === piece;
                        // 編集モード時は0個でもドラッグ可能（ストックとして機能）
                        const canDrag = (count > 0 || isEditMode) && Boolean(onPiecePointerDown);
                        const canSelect = count > 0 && isActive;
                        const isDisabled = !canDrag && !canSelect && !isEditMode;
                        const maxCount = PIECE_CAP[piece];

                        // 対局中は持っている駒だけ詰めて表示
                        // 対局前（!isMatchRunning）は編集のために全ての駒を表示する
                        if (isMatchRunning && !isEditMode && count === 0) {
                            return null;
                        }

                        return (
                            <div key={`${owner}-${piece}`} className="flex items-center gap-px">
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
                                        className={cn(
                                            "flex flex-col gap-px",
                                            isEditMode ? "visible" : "invisible",
                                        )}
                                    >
                                        <button
                                            type="button"
                                            onClick={() => onIncrement?.(piece)}
                                            disabled={!isEditMode || count >= maxCount}
                                            aria-label={`${PIECE_LABELS[piece]}を増やす`}
                                            className={cn(
                                                "flex h-4 w-5 items-center justify-center rounded-t border border-b-0 border-[hsl(var(--border))] text-xs font-bold leading-none",
                                                count < maxCount
                                                    ? "cursor-pointer bg-[hsl(var(--wafuu-washi))] text-[hsl(var(--wafuu-sumi))] opacity-100"
                                                    : "cursor-not-allowed bg-[hsl(var(--muted))] text-[hsl(var(--muted-foreground))] opacity-40",
                                            )}
                                        >
                                            +
                                        </button>
                                        <button
                                            type="button"
                                            onClick={() => onDecrement?.(piece)}
                                            disabled={!isEditMode || count <= 0}
                                            aria-label={`${PIECE_LABELS[piece]}を減らす`}
                                            className={cn(
                                                "flex h-4 w-5 items-center justify-center rounded-b border border-[hsl(var(--border))] text-xs font-bold leading-none",
                                                count > 0
                                                    ? "cursor-pointer bg-[hsl(var(--wafuu-washi))] text-[hsl(var(--wafuu-sumi))] opacity-100"
                                                    : "cursor-not-allowed bg-[hsl(var(--muted))] text-[hsl(var(--muted-foreground))] opacity-40",
                                            )}
                                        >
                                            −
                                        </button>
                                    </div>
                                )}
                            </div>
                        );
                    })}
                </div>
            </div>
        </div>
    );
}
