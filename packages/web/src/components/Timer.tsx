import { Card } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { useGameStore } from "@/stores/gameStore";
import { useEffect } from "react";

interface TimerProps {
    player: "black" | "white";
    className?: string;
}

export function Timer({ player, className }: TimerProps) {
    const { timer, tick } = useGameStore();
    const { config, activePlayer } = timer;

    // Timer tick effect
    useEffect(() => {
        if (!config.mode || timer.isPaused || !activePlayer || timer.hasTimedOut) {
            return;
        }

        const interval = setInterval(() => {
            tick();
        }, 100); // Update every 100ms

        return () => clearInterval(interval);
    }, [config.mode, timer.isPaused, activePlayer, timer.hasTimedOut, tick]);

    if (!config.mode) {
        return null;
    }

    const time = player === "black" ? timer.blackTime : timer.whiteTime;
    const inByoyomi = player === "black" ? timer.blackInByoyomi : timer.whiteInByoyomi;
    const warningLevel = player === "black" ? timer.blackWarningLevel : timer.whiteWarningLevel;
    const isActive = activePlayer === player;

    // Format time display
    const totalSeconds = Math.floor(time / 1000);
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    const deciseconds = Math.floor((time % 1000) / 100);

    let timeDisplay = `${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}.${deciseconds}`;

    if (config.mode === "perMove") {
        timeDisplay = `${totalSeconds}.${deciseconds}秒`;
    }

    return (
        <Card
            className={cn(
                "p-4 transition-all",
                isActive && "bg-primary text-primary-foreground shadow-lg",
                warningLevel === "warning" && "border-yellow-500",
                warningLevel === "critical" && "border-red-500 animate-pulse",
                className,
            )}
        >
            <div className="text-center">
                <div className="text-sm font-medium mb-1">
                    {player === "black" ? "先手" : "後手"}
                    {inByoyomi && " (秒読み)"}
                </div>
                <div
                    className={cn(
                        "font-mono text-2xl font-bold",
                        warningLevel === "critical" && "text-red-500",
                    )}
                >
                    {timeDisplay}
                </div>
            </div>
        </Card>
    );
}
