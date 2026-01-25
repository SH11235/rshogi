/**
 * ドラッグ中のゴースト駒コンポーネント
 *
 * Portal で document.body に描画し、transform で高速移動
 * pointer-events: none でクリックを透過
 */

import type { PieceType } from "@shogi/app-core";
import { cn } from "@shogi/design-system";
import { forwardRef } from "react";
import { createPortal } from "react-dom";
import type { DndState } from "./types";

const PIECE_LABELS: Record<PieceType, string> = {
    K: "玉",
    R: "飛",
    B: "角",
    G: "金",
    S: "銀",
    N: "桂",
    L: "香",
    P: "歩",
};

interface DragGhostProps {
    /** DnD 状態 */
    dndState: DndState;
    /** オーナーの向き */
    ownerOrientation?: "sente" | "gote";
}

/**
 * ゴースト駒コンポーネント
 *
 * 和風デザインで、ドラッグ中の駒を表現
 * - 木目調の背景
 * - ドロップシャドウで浮遊感
 * - 削除モード時は赤いオーラ
 */
export const DragGhost = forwardRef<HTMLDivElement, DragGhostProps>(function DragGhost(
    { dndState, ownerOrientation = "sente" },
    ref,
) {
    const { isDragging, payload, mode } = dndState;

    if (typeof document === "undefined") {
        return null;
    }

    const shouldFlip =
        ownerOrientation === "sente" ? payload?.owner === "gote" : payload?.owner === "sente";

    return createPortal(
        <div
            ref={ref}
            className={cn(
                "pointer-events-none fixed left-0 top-0 z-[9999]",
                "flex h-12 w-12 items-center justify-center",
                "transition-opacity duration-75",
                isDragging ? "opacity-100" : "opacity-0",
            )}
            style={{
                display: isDragging ? "flex" : "none",
                willChange: "transform",
            }}
            aria-hidden="true"
        >
            {/* 駒本体 */}
            <div
                className={cn(
                    "relative flex h-11 w-11 items-center justify-center",
                    "rounded-lg border border-shogi-outer-border",
                    "bg-[radial-gradient(circle_at_30%_20%,hsl(var(--shogi-piece-bg)),hsl(var(--shogi-piece-bg-dark)))]",
                    "shadow-[0_8px_24px_rgba(0,0,0,0.35),0_4px_8px_rgba(0,0,0,0.2)]",
                    "transform-gpu",
                    shouldFlip && "-rotate-180",
                    // 削除モード時のエフェクト
                    mode === "delete" && [
                        "ring-2 ring-red-500/70",
                        "shadow-[0_0_16px_rgba(239,68,68,0.5),0_8px_24px_rgba(0,0,0,0.35)]",
                    ],
                )}
            >
                {/* 駒文字 */}
                {payload && (
                    <span
                        className={cn(
                            "text-lg font-bold leading-none tracking-tight",
                            "text-shogi-piece-text",
                            "drop-shadow-[0_1px_1px_rgba(255,255,255,0.8)]",
                        )}
                    >
                        {PIECE_LABELS[payload.pieceType]}
                    </span>
                )}

                {/* 成りマーク */}
                {payload?.isPromoted && (
                    <span
                        className={cn(
                            "absolute -right-0.5 -top-0.5",
                            "rounded-full bg-wafuu-shu px-1",
                            "text-[8px] font-bold text-white",
                            "shadow-sm",
                        )}
                    >
                        成
                    </span>
                )}

                {/* 木目テクスチャオーバーレイ */}
                <div
                    className={cn(
                        "pointer-events-none absolute inset-0 rounded-lg",
                        "bg-[repeating-linear-gradient(90deg,transparent,transparent_2px,rgba(139,90,43,0.03)_2px,rgba(139,90,43,0.03)_4px)]",
                        "opacity-50",
                    )}
                />
            </div>

            {/* ドラッグ中のパルスエフェクト */}
            <div
                className={cn(
                    "absolute inset-0 rounded-lg",
                    "animate-ping",
                    mode === "delete" ? "bg-red-400/20" : "bg-amber-400/20",
                    "pointer-events-none",
                )}
                style={{
                    animationDuration: "1.5s",
                    animationIterationCount: "infinite",
                }}
            />
        </div>,
        document.body,
    );
});
