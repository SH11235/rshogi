import { cn } from "@shogi/design-system";
import type { ReactElement } from "react";

export interface MobileNavigationProps {
    /** 現在の手数 */
    currentPly: number;
    /** 総手数 */
    totalPly: number;
    /** 戻るボタン */
    onBack: () => void;
    /** 進むボタン */
    onForward: () => void;
    /** 最初へ */
    onToStart: () => void;
    /** 最後へ */
    onToEnd: () => void;
    /** 無効状態 */
    disabled?: boolean;
}

/**
 * スマホ検討モード用のナビゲーションボタン
 */
export function MobileNavigation({
    currentPly,
    totalPly,
    onBack,
    onForward,
    onToStart,
    onToEnd,
    disabled = false,
}: MobileNavigationProps): ReactElement {
    const canGoBack = currentPly > 0;
    const canGoForward = currentPly < totalPly;

    return (
        <div className="flex items-center justify-center gap-2 py-2">
            {/* 最初へ */}
            <button
                type="button"
                onClick={onToStart}
                disabled={disabled || !canGoBack}
                className={cn(
                    "w-10 h-10 flex items-center justify-center rounded-lg text-lg",
                    "border border-border bg-background",
                    "transition-colors",
                    disabled || !canGoBack
                        ? "opacity-40 cursor-not-allowed"
                        : "hover:bg-muted active:bg-muted/80",
                )}
                title="最初へ"
            >
                ⏮
            </button>

            {/* 1手戻る */}
            <button
                type="button"
                onClick={onBack}
                disabled={disabled || !canGoBack}
                className={cn(
                    "w-14 h-10 flex items-center justify-center rounded-lg text-base font-medium",
                    "border border-border bg-background",
                    "transition-colors",
                    disabled || !canGoBack
                        ? "opacity-40 cursor-not-allowed"
                        : "hover:bg-muted active:bg-muted/80",
                )}
                title="1手戻る"
            >
                ◀ 前
            </button>

            {/* 手数表示 */}
            <div className="min-w-[60px] text-center text-sm font-medium">
                {currentPly} / {totalPly}
            </div>

            {/* 1手進む */}
            <button
                type="button"
                onClick={onForward}
                disabled={disabled || !canGoForward}
                className={cn(
                    "w-14 h-10 flex items-center justify-center rounded-lg text-base font-medium",
                    "border border-border bg-background",
                    "transition-colors",
                    disabled || !canGoForward
                        ? "opacity-40 cursor-not-allowed"
                        : "hover:bg-muted active:bg-muted/80",
                )}
                title="1手進む"
            >
                次 ▶
            </button>

            {/* 最後へ */}
            <button
                type="button"
                onClick={onToEnd}
                disabled={disabled || !canGoForward}
                className={cn(
                    "w-10 h-10 flex items-center justify-center rounded-lg text-lg",
                    "border border-border bg-background",
                    "transition-colors",
                    disabled || !canGoForward
                        ? "opacity-40 cursor-not-allowed"
                        : "hover:bg-muted active:bg-muted/80",
                )}
                title="最後へ"
            >
                ⏭
            </button>
        </div>
    );
}
