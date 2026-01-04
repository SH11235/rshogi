import { cn } from "@shogi/design-system";
import type { ReactElement } from "react";

export interface EvalBarProps {
    /** 評価値（先手有利でプラス、後手有利でマイナス） */
    evalCp?: number;
    /** 詰み手数（先手勝ちでプラス、後手勝ちでマイナス） */
    evalMate?: number;
    /** 評価値がない場合の表示 */
    noEvalText?: string;
}

/**
 * スマホ用評価値バー
 * 先手有利なら右側が伸び、後手有利なら左側が伸びる
 */
export function EvalBar({ evalCp, evalMate, noEvalText = "-" }: EvalBarProps): ReactElement {
    // 評価値をパーセンテージに変換（-100% 〜 +100%）
    // 800cp（8点）で100%とする
    const getPercentage = (): number => {
        if (evalMate !== undefined) {
            // 詰みの場合は100%
            return evalMate > 0 ? 100 : -100;
        }
        if (evalCp !== undefined) {
            // 800cpを100%として、-100〜100の範囲に収める
            const clamped = Math.max(-800, Math.min(800, evalCp));
            return (clamped / 800) * 100;
        }
        return 0;
    };

    const percentage = getPercentage();
    const hasEval = evalCp !== undefined || evalMate !== undefined;

    // 表示テキスト
    const getDisplayText = (): string => {
        if (evalMate !== undefined) {
            return evalMate > 0 ? `詰み${evalMate}手` : `詰まされ${Math.abs(evalMate)}手`;
        }
        if (evalCp !== undefined) {
            const sign = evalCp > 0 ? "+" : "";
            return `${sign}${evalCp}`;
        }
        return noEvalText;
    };

    // 先手有利（プラス）の幅
    const senteWidth = percentage > 0 ? Math.abs(percentage) : 0;
    // 後手有利（マイナス）の幅
    const goteWidth = percentage < 0 ? Math.abs(percentage) : 0;

    return (
        <div className="flex items-center gap-2 px-2 h-8">
            {/* バー */}
            <div className="flex-1 h-5 relative bg-muted rounded overflow-hidden">
                {/* 中央線 */}
                <div className="absolute left-1/2 top-0 bottom-0 w-px bg-border z-10" />

                {/* 後手側（左半分） */}
                <div
                    className="absolute right-1/2 top-0 bottom-0 flex justify-end"
                    style={{ width: "50%" }}
                >
                    <div
                        className={cn("h-full transition-all duration-300", "bg-wafuu-ai/70")}
                        style={{ width: `${goteWidth}%` }}
                    />
                </div>

                {/* 先手側（右半分） */}
                <div className="absolute left-1/2 top-0 bottom-0" style={{ width: "50%" }}>
                    <div
                        className={cn("h-full transition-all duration-300", "bg-wafuu-shu/70")}
                        style={{ width: `${senteWidth}%` }}
                    />
                </div>
            </div>

            {/* 評価値表示 */}
            <div
                className={cn(
                    "min-w-[70px] text-right text-sm font-mono tabular-nums",
                    hasEval ? "text-foreground" : "text-muted-foreground",
                )}
            >
                {getDisplayText()}
            </div>
        </div>
    );
}
