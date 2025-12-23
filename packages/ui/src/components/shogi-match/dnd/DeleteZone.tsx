/**
 * 削除ゾーンコンポーネント
 *
 * ドラッグした駒をドロップして削除するためのエリア
 * ドラッグ中のみ表示され、ホバー時に視覚的フィードバック
 */

import { cn } from "@shogi/design-system";
import { forwardRef } from "react";
import type { DndState } from "./types";

export interface DeleteZoneProps {
    /** DnD 状態 */
    dndState: DndState;
    /** クラス名 */
    className?: string;
}

/**
 * 削除ゾーン
 *
 * 和風デザインで、駒を削除するためのドロップエリア
 * - 通常時は半透明
 * - ホバー時は赤くハイライト
 * - ゴミ箱アイコンではなく和風の「×」マーク
 */
export const DeleteZone = forwardRef<HTMLDivElement, DeleteZoneProps>(function DeleteZone(
    { dndState, className },
    ref,
) {
    const { isDragging, mode } = dndState;
    const isHovering = mode === "delete";

    return (
        <div
            ref={ref}
            className={cn(
                "relative overflow-hidden",
                "rounded-xl border-2 border-dashed",
                "flex flex-col items-center justify-center gap-1",
                "transition-all duration-200 ease-out",
                "select-none",
                // 通常時（ドラッグ中）
                isDragging &&
                    !isHovering && ["border-[#9a7b4a]/50", "bg-[#f5ebe0]/50", "text-[#9a7b4a]/70"],
                // ホバー時
                isHovering && [
                    "border-red-400",
                    "bg-gradient-to-br from-red-50 to-red-100",
                    "text-red-600",
                    "shadow-[0_0_20px_rgba(239,68,68,0.3)]",
                    "scale-105",
                ],
                // 非ドラッグ時
                !isDragging && ["border-transparent", "bg-transparent", "opacity-0"],
                className,
            )}
            aria-label="駒を削除"
            role="region"
        >
            {/* 和風の×マーク */}
            <div
                className={cn(
                    "relative h-8 w-8",
                    "transition-transform duration-200",
                    isHovering && "scale-110",
                )}
            >
                {/* 左斜線 */}
                <div
                    className={cn(
                        "absolute left-1/2 top-1/2 h-1 w-8",
                        "-translate-x-1/2 -translate-y-1/2 rotate-45",
                        "rounded-full",
                        isHovering ? "bg-red-500" : "bg-[#9a7b4a]/60",
                        "transition-colors duration-200",
                    )}
                />
                {/* 右斜線 */}
                <div
                    className={cn(
                        "absolute left-1/2 top-1/2 h-1 w-8",
                        "-translate-x-1/2 -translate-y-1/2 -rotate-45",
                        "rounded-full",
                        isHovering ? "bg-red-500" : "bg-[#9a7b4a]/60",
                        "transition-colors duration-200",
                    )}
                />
            </div>

            {/* ラベル */}
            <span
                className={cn(
                    "text-xs font-medium tracking-wide",
                    "transition-colors duration-200",
                )}
            >
                {isHovering ? "離して削除" : "削除"}
            </span>

            {/* ホバー時の波紋エフェクト */}
            {isHovering && (
                <div
                    className={cn(
                        "absolute inset-0",
                        "animate-pulse",
                        "bg-red-400/10",
                        "pointer-events-none",
                    )}
                />
            )}

            {/* 角の装飾（和風） */}
            <div
                className={cn(
                    "absolute left-2 top-2 h-3 w-3",
                    "border-l-2 border-t-2",
                    isHovering ? "border-red-400" : "border-[#9a7b4a]/40",
                    "transition-colors duration-200",
                )}
            />
            <div
                className={cn(
                    "absolute right-2 top-2 h-3 w-3",
                    "border-r-2 border-t-2",
                    isHovering ? "border-red-400" : "border-[#9a7b4a]/40",
                    "transition-colors duration-200",
                )}
            />
            <div
                className={cn(
                    "absolute bottom-2 left-2 h-3 w-3",
                    "border-b-2 border-l-2",
                    isHovering ? "border-red-400" : "border-[#9a7b4a]/40",
                    "transition-colors duration-200",
                )}
            />
            <div
                className={cn(
                    "absolute bottom-2 right-2 h-3 w-3",
                    "border-b-2 border-r-2",
                    isHovering ? "border-red-400" : "border-[#9a7b4a]/40",
                    "transition-colors duration-200",
                )}
            />
        </div>
    );
});
