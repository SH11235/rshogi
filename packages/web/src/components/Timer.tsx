import { cn } from "@/lib/utils";
import { formatTime } from "@/types/timer";
import type { Player } from "shogi-core";

interface TimerProps {
    /** Player this timer is for */
    player: Player;
    /** Main time remaining in milliseconds */
    mainTime: number;
    /** Byoyomi time remaining in milliseconds (null if not in byoyomi) */
    byoyomiTime: number | null;
    /** Whether this timer is currently active/counting down */
    isActive: boolean;
    /** Whether the timer is enabled in settings */
    isEnabled: boolean;
    /** Whether the game has ended due to timeout */
    hasTimedOut: boolean;
    /** The player who timed out (if any) */
    timedOutPlayer: Player | null;
    /** Additional CSS classes */
    className?: string;
}

export function Timer({
    player,
    mainTime,
    byoyomiTime,
    isActive,
    isEnabled,
    hasTimedOut,
    timedOutPlayer,
    className,
}: TimerProps) {
    if (!isEnabled) {
        return null;
    }

    const isTimedOut = hasTimedOut && timedOutPlayer === player;
    const isInByoyomi = byoyomiTime !== null;
    const displayTime = isInByoyomi ? byoyomiTime : mainTime;
    const isLowTime = displayTime < 30000; // Less than 30 seconds
    const isCriticalTime = displayTime < 10000; // Less than 10 seconds

    const playerLabel = player === "black" ? "先手" : "後手";

    return (
        <div
            className={cn(
                "flex flex-col items-center p-3 rounded-lg border transition-all duration-200",
                isActive && !isTimedOut && "ring-2 ring-blue-500 border-blue-500 bg-blue-50",
                isTimedOut && "ring-2 ring-red-500 border-red-500 bg-red-50",
                !isActive && !isTimedOut && "border-gray-300 bg-gray-50",
                className,
            )}
        >
            {/* Player label */}
            <div className="text-sm font-medium text-gray-600 mb-2">{playerLabel}</div>

            {/* Timer display */}
            <div
                className={cn(
                    "text-2xl font-mono font-bold transition-colors",
                    isTimedOut && "text-red-600",
                    !isTimedOut && isActive && isCriticalTime && "text-red-500 animate-pulse",
                    !isTimedOut && isActive && isLowTime && !isCriticalTime && "text-orange-500",
                    !isTimedOut && isActive && !isLowTime && "text-blue-600",
                    !isTimedOut && !isActive && "text-gray-700",
                )}
            >
                {isTimedOut ? "時間切れ" : formatTime(displayTime)}
            </div>

            {/* Byoyomi indicator */}
            {isInByoyomi && !isTimedOut && (
                <div className="text-xs text-orange-600 font-medium mt-1">秒読み</div>
            )}

            {/* Time status indicator */}
            {!isTimedOut && mainTime <= 0 && !isInByoyomi && (
                <div className="text-xs text-gray-500 mt-1">本時終了</div>
            )}
        </div>
    );
}
