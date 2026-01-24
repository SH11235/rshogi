import type { Player } from "@shogi/app-core";
import { cn } from "@shogi/design-system";
import type { ReactElement } from "react";

type IconSize = "xs" | "sm" | "md" | "lg" | "xl";

const SIZE_CONFIG: Record<IconSize, { icon: string; text: string }> = {
    xs: { icon: "w-4 h-4", text: "text-sm" },
    sm: { icon: "w-5 h-5", text: "text-base" },
    md: { icon: "w-6 h-6", text: "text-lg" },
    lg: { icon: "w-8 h-8", text: "text-xl" },
    xl: { icon: "w-10 h-10", text: "text-2xl" },
};

interface PlayerIconProps {
    /** プレイヤー（先手/後手） */
    side: Player;
    /** AIプレイヤーかどうか */
    isAI?: boolean;
    /** アイコンサイズ */
    size?: IconSize;
    /** 追加のクラス名 */
    className?: string;
    /** AI時に色付き枠を表示するか（デフォルト: true） */
    showBorder?: boolean;
}

/**
 * プレイヤーアイコン
 * - 人間: ☗（先手）/ ☖（後手）を色付きで表示
 * - AI: ラムアイコンを色付き枠で表示（showBorder=falseで枠なし）
 */
export function PlayerIcon({
    side,
    isAI = false,
    size = "md",
    className,
    showBorder = true,
}: PlayerIconProps): ReactElement {
    const config = SIZE_CONFIG[size];
    const colorClass = side === "sente" ? "text-wafuu-shu" : "text-wafuu-ai";
    const borderColorClass = side === "sente" ? "ring-wafuu-shu" : "ring-wafuu-ai";
    const marker = side === "sente" ? "☗" : "☖";

    if (isAI) {
        return (
            <img
                src="/ramu.jpeg"
                alt={side === "sente" ? "先手AI" : "後手AI"}
                title={side === "sente" ? "先手" : "後手"}
                className={cn(
                    "rounded-full object-cover",
                    config.icon,
                    showBorder && "ring-2",
                    showBorder && borderColorClass,
                    className,
                )}
            />
        );
    }

    return (
        <span
            className={cn("font-bold select-none", config.text, colorClass, className)}
            title={side === "sente" ? "先手" : "後手"}
        >
            {marker}
        </span>
    );
}
