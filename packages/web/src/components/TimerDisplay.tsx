import { cn } from "@/lib/utils";
import { useGameStore } from "@/stores/gameStore";
import {
    type WarningLevel,
    formatByoyomiTime,
    formatConsiderationTime,
    formatTimeWithHours,
} from "@/types/timer";
import { memo, useEffect } from "react";
import type { Player } from "shogi-core";
import { Button } from "./ui/button";

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
    const useConsideration = useGameStore((state) => state.useConsideration);
    const cancelConsideration = useGameStore((state) => state.cancelConsideration);

    const time = player === "black" ? timer.blackTime : timer.whiteTime;
    const inByoyomi = player === "black" ? timer.blackInByoyomi : timer.whiteInByoyomi;
    const warningLevel = player === "black" ? timer.blackWarningLevel : timer.whiteWarningLevel;
    const isActive = timer.activePlayer === player;
    const considerationsRemaining =
        player === "black"
            ? timer.blackConsiderationsRemaining
            : timer.whiteConsiderationsRemaining;
    const isConsiderationMode = timer.config.mode === "consideration";

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
        <div className="space-y-1">
            <div
                className={cn(
                    "text-lg font-mono",
                    isActive && "font-bold",
                    getWarningClass(warningLevel),
                )}
            >
                {inByoyomi ? formatByoyomiTime(time) : formatTimeWithHours(time)}
            </div>
            {isConsiderationMode && considerationsRemaining > 0 && (
                <div className="text-sm space-y-1">
                    <div className="text-gray-600">
                        {formatConsiderationTime(considerationsRemaining)}
                    </div>
                    {isActive && !timer.isPaused && (
                        <Button
                            size="sm"
                            variant={timer.isUsingConsideration ? "secondary" : "outline"}
                            onClick={
                                timer.isUsingConsideration ? cancelConsideration : useConsideration
                            }
                            className="w-full text-xs"
                        >
                            {timer.isUsingConsideration ? "考慮時間キャンセル" : "考慮時間使用"}
                        </Button>
                    )}
                </div>
            )}
        </div>
    );
});

TimerDisplay.displayName = "TimerDisplay";
