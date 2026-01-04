import type { Player } from "@shogi/app-core";
import { cn } from "@shogi/design-system";
import type { ReactElement } from "react";
import type { TickState } from "../hooks/useClockManager";
import { formatTime } from "../utils/timeFormat";
import type { SideSetting } from "./MatchSettingsPanel";

interface MobileClockDisplayProps {
    /** 時計の状態 */
    clocks: TickState;
    /** 先手・後手の設定 */
    sides: { sente: SideSetting; gote: SideSetting };
}

/**
 * モバイル用コンパクトクロック表示
 * 対局中に盤面の上に横並びで表示
 */
export function MobileClockDisplay({ clocks, sides }: MobileClockDisplayProps): ReactElement {
    const renderClock = (side: Player) => {
        const clock = clocks[side];
        const ticking = clocks.ticking === side;
        const isHuman = sides[side].role === "human";
        const sideMarker = side === "sente" ? "☗" : "☖";
        const colorClass = side === "sente" ? "text-wafuu-shu" : "text-wafuu-ai";

        return (
            <div
                className={cn(
                    "flex items-center gap-1.5 px-2 py-1 rounded-lg",
                    ticking ? "bg-primary/10 ring-1 ring-primary/30" : "bg-muted/50",
                )}
            >
                <span className={cn("font-bold text-sm", colorClass)}>{sideMarker}</span>
                <span className="text-xs text-muted-foreground">{isHuman ? "人" : "AI"}</span>
                <span className="font-mono text-sm tabular-nums">
                    {formatTime(clock.mainMs)}
                    <span className="text-muted-foreground">+</span>
                    {formatTime(clock.byoyomiMs)}
                </span>
                {ticking && <span className="w-2 h-2 rounded-full bg-primary animate-pulse" />}
            </div>
        );
    };

    return (
        <div className="flex items-center justify-center gap-2 py-1">
            {renderClock("sente")}
            {renderClock("gote")}
        </div>
    );
}
