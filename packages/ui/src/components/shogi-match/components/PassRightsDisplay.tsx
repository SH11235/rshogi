/**
 * パス権表示コンポーネント
 *
 * 残りパス権を視覚的に表示する。
 * - ドット表示: 残っているパス権は塗りつぶし、使用済みは空洞
 * - 手番側はハイライト表示
 * - モバイル用のコンパクト表示にも対応
 */

import { cn } from "@shogi/design-system";
import type { ReactElement } from "react";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "../../tooltip";

interface PassRightsDisplayProps {
    /** 残りパス権数 */
    remaining: number;
    /** 最大パス権数（表示用） */
    max: number;
    /** 手番側かどうか */
    isActive?: boolean;
    /** コンパクト表示（モバイル用） */
    compact?: boolean;
    /** 追加のクラス名 */
    className?: string;
}

/**
 * パス権をドット形式で表示するコンポーネント
 */
export function PassRightsDisplay({
    remaining,
    max,
    isActive = false,
    compact = false,
    className,
}: PassRightsDisplayProps): ReactElement | null {
    // パス権が0の場合は非表示
    if (max === 0) {
        return null;
    }

    // ドットのレンダリング（固定ID配列を使用してlintエラーを回避）
    const DOT_IDS = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"] as const;
    const dots = DOT_IDS.slice(0, max).map((id, idx) => {
        const isFilled = idx < remaining;
        return (
            <span
                key={id}
                className={cn(
                    "inline-block rounded-full",
                    compact ? "h-2 w-2" : "h-2.5 w-2.5",
                    isFilled
                        ? isActive
                            ? "bg-primary"
                            : "bg-foreground/70"
                        : "border border-foreground/30 bg-transparent",
                )}
            />
        );
    });

    const content = (
        <div
            className={cn(
                "flex items-center gap-1",
                isActive && "opacity-100",
                !isActive && "opacity-60",
                className,
            )}
        >
            <span className={cn("text-xs text-muted-foreground", compact && "sr-only")}>パス</span>
            <div className="flex items-center gap-0.5">{dots}</div>
            {!compact && <span className="text-xs text-muted-foreground">({remaining})</span>}
        </div>
    );

    // コンパクト表示の場合はツールチップで詳細を表示
    if (compact) {
        return (
            <TooltipProvider>
                <Tooltip>
                    <TooltipTrigger asChild>
                        <div className="cursor-help">{content}</div>
                    </TooltipTrigger>
                    <TooltipContent>
                        <p>
                            パス権: {remaining}/{max}
                        </p>
                    </TooltipContent>
                </Tooltip>
            </TooltipProvider>
        );
    }

    return content;
}
