import { cn } from "@/lib/utils";
import { useGameStore } from "@/stores/gameStore";
import { type WarningLevel, formatByoyomiTime, formatTimeWithHours } from "@/types/timer";
import { memo, useEffect } from "react";
import type { Player } from "shogi-core";

interface TimerDisplayProps {
    player: Player;
}

function getWarningClass(warningLevel: WarningLevel): string {
    switch (warningLevel) {
        case "critical":
            return "text-red-600 font-bold animate-pulse";
        case "warning":
            return "text-yellow-600";
        default:
            return "";
    }
}

export const TimerDisplay = memo(({ player }: TimerDisplayProps) => {
    const timer = useGameStore((state) => state.timer);
    const tick = useGameStore((state) => state.tick);

    const time = player === "black" ? timer.blackTime : timer.whiteTime;
    const inByoyomi = player === "black" ? timer.blackInByoyomi : timer.whiteInByoyomi;
    const warningLevel = player === "black" ? timer.blackWarningLevel : timer.whiteWarningLevel;
    const isActive = timer.activePlayer === player;

    // タイマーのtick処理
    useEffect(() => {
        if (timer.config.mode && isActive && !timer.isPaused && !timer.hasTimedOut) {
            const interval = setInterval(tick, 100);
            return () => clearInterval(interval);
        }
    }, [timer.config.mode, isActive, timer.isPaused, timer.hasTimedOut, tick]);

    // タイマーが設定されていない場合は何も表示しない
    if (!timer.config.mode) {
        return null;
    }

    return (
        <div
            className={cn(
                "text-lg font-mono",
                isActive && "font-bold",
                getWarningClass(warningLevel),
            )}
        >
            {inByoyomi ? formatByoyomiTime(time) : formatTimeWithHours(time)}
        </div>
    );
});

TimerDisplay.displayName = "TimerDisplay";
