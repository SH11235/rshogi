import { useIntegratedTimer } from "@/hooks/useIntegratedTimer";
import { cn } from "@/lib/utils";
import { Timer } from "./Timer";

interface TimerDisplayProps {
    /** Additional CSS classes */
    className?: string;
    /** Callback when a player times out */
    onTimeout?: (player: "black" | "white") => void;
}

export function TimerDisplay({ className, onTimeout }: TimerDisplayProps) {
    const timer = useIntegratedTimer({ onTimeout });

    // Don't render if timer is disabled
    if (!timer.config.enabled) {
        return null;
    }

    return (
        <div className={cn("w-full max-w-md mx-auto", className)}>
            {/* Timer header */}
            <div className="text-center mb-3">
                <h3 className="text-lg font-semibold text-gray-800">⏱️ 対局時計</h3>
                {timer.hasTimedOut && (
                    <div className="text-sm text-red-600 font-medium mt-1">
                        {timer.timedOutPlayer === "black" ? "先手" : "後手"}の時間切れ
                    </div>
                )}
            </div>

            {/* Timers container */}
            <div className="grid grid-cols-2 gap-4">
                {/* Black (先手) timer */}
                <Timer
                    player="black"
                    mainTime={timer.blackMainTime}
                    byoyomiTime={timer.activePlayer === "black" ? timer.byoyomiTime : null}
                    isActive={timer.isRunning && timer.activePlayer === "black"}
                    isEnabled={timer.config.enabled}
                    hasTimedOut={timer.hasTimedOut}
                    timedOutPlayer={timer.timedOutPlayer}
                />

                {/* White (後手) timer */}
                <Timer
                    player="white"
                    mainTime={timer.whiteMainTime}
                    byoyomiTime={timer.activePlayer === "white" ? timer.byoyomiTime : null}
                    isActive={timer.isRunning && timer.activePlayer === "white"}
                    isEnabled={timer.config.enabled}
                    hasTimedOut={timer.hasTimedOut}
                    timedOutPlayer={timer.timedOutPlayer}
                />
            </div>

            {/* Timer configuration display */}
            {timer.config.enabled && (
                <div className="mt-3 text-center text-sm text-gray-600">
                    持ち時間: {Math.floor(timer.config.mainTimeMs / 60000)}分
                    {timer.config.byoyomiMs > 0 && (
                        <span> / 秒読み: {Math.floor(timer.config.byoyomiMs / 1000)}秒</span>
                    )}
                </div>
            )}

            {/* Timer controls for testing/debugging (only in development) */}
            {process.env.NODE_ENV === "development" && timer.config.enabled && (
                <div className="mt-4 p-3 border rounded-lg bg-yellow-50">
                    <div className="text-xs text-yellow-800 font-medium mb-2">
                        開発用コントロール
                    </div>
                    <div className="flex flex-wrap gap-2">
                        <button
                            type="button"
                            onClick={() => timer.manualStart("black")}
                            className="px-2 py-1 text-xs bg-blue-500 text-white rounded"
                            disabled={timer.isRunning}
                        >
                            開始
                        </button>
                        <button
                            type="button"
                            onClick={timer.manualPause}
                            className="px-2 py-1 text-xs bg-orange-500 text-white rounded"
                            disabled={!timer.isRunning}
                        >
                            一時停止
                        </button>
                        <button
                            type="button"
                            onClick={timer.manualResume}
                            className="px-2 py-1 text-xs bg-green-500 text-white rounded"
                            disabled={timer.isRunning}
                        >
                            再開
                        </button>
                        <button
                            type="button"
                            onClick={timer.manualSwitchPlayer}
                            className="px-2 py-1 text-xs bg-purple-500 text-white rounded"
                            disabled={!timer.isRunning}
                        >
                            切替
                        </button>
                        <button
                            type="button"
                            onClick={timer.manualReset}
                            className="px-2 py-1 text-xs bg-gray-500 text-white rounded"
                        >
                            リセット
                        </button>
                    </div>
                </div>
            )}
        </div>
    );
}
