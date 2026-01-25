import type { Player } from "@shogi/app-core";
import { cn } from "@shogi/design-system";
import { type ReactElement, useState } from "react";
import { Dialog, DialogContent, DialogTitle } from "../../dialog";

type IconSize = "xs" | "sm" | "md" | "lg" | "xl";

const SIZE_CONFIG: Record<IconSize, { icon: string; text: string; container: string }> = {
    xs: { icon: "w-4 h-4", text: "text-sm", container: "w-4 h-4" },
    sm: { icon: "w-5 h-5", text: "text-base", container: "w-5 h-5" },
    md: { icon: "w-6 h-6", text: "text-lg", container: "w-6 h-6" },
    lg: { icon: "w-8 h-8", text: "text-xl", container: "w-8 h-8" },
    xl: { icon: "w-10 h-10", text: "text-2xl", container: "w-10 h-10" },
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
    /** クリックで拡大表示を有効にするか（AI時のみ有効） */
    enableZoom?: boolean;
}

/**
 * プレイヤーアイコン
 * - 人間: ☗（先手）/ ☖（後手）を色付きで表示
 * - AI: ラムアイコンを色付き枠で表示（showBorder=falseで枠なし）
 * - enableZoom=trueの場合、AIアイコンクリックで拡大表示
 */
export function PlayerIcon({
    side,
    isAI = false,
    size = "md",
    className,
    showBorder = true,
    enableZoom = false,
}: PlayerIconProps): ReactElement {
    const [isZoomOpen, setIsZoomOpen] = useState(false);
    const config = SIZE_CONFIG[size];
    const colorClass = side === "sente" ? "text-wafuu-shu" : "text-wafuu-ai";
    const borderColorClass = side === "sente" ? "ring-wafuu-shu" : "ring-wafuu-ai";
    const marker = side === "sente" ? "☗" : "☖";
    const aiAlt = side === "sente" ? "先手AI" : "後手AI";
    const aiTitle = side === "sente" ? "先手" : "後手";
    const aiIconSrc = "/ramu.jpeg";

    if (isAI) {
        const canZoom = enableZoom;
        const aiImage = (
            <img
                src={aiIconSrc}
                alt={aiAlt}
                title={aiTitle}
                className={cn(
                    "rounded-full object-cover",
                    config.icon,
                    showBorder && "ring-2",
                    showBorder && borderColorClass,
                    canZoom && "cursor-pointer hover:opacity-80 transition-opacity",
                    className,
                )}
            />
        );
        return (
            <>
                {canZoom ? (
                    <button
                        type="button"
                        className={cn(
                            "inline-flex rounded-full border-0 bg-transparent p-0 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-background",
                            borderColorClass,
                        )}
                        onClick={() => setIsZoomOpen(true)}
                        aria-label={`${aiAlt}を拡大表示`}
                        aria-expanded={isZoomOpen}
                    >
                        {aiImage}
                    </button>
                ) : (
                    aiImage
                )}
                {canZoom && (
                    <Dialog open={isZoomOpen} onOpenChange={setIsZoomOpen}>
                        <DialogContent
                            style={{
                                width: "auto",
                                maxWidth: "min(90vw, 400px)",
                                padding: "16px",
                            }}
                        >
                            <DialogTitle className="sr-only">{aiAlt}を拡大表示</DialogTitle>
                            <div className="flex flex-col items-center gap-3">
                                <img
                                    src={aiIconSrc}
                                    alt="ラム"
                                    className="w-full max-w-[360px] rounded-lg object-cover"
                                />
                            </div>
                        </DialogContent>
                    </Dialog>
                )}
            </>
        );
    }

    return (
        <span
            className={cn(
                "inline-flex items-center justify-center font-bold select-none",
                config.container,
                config.text,
                colorClass,
                className,
            )}
            title={side === "sente" ? "先手" : "後手"}
        >
            {marker}
        </span>
    );
}
