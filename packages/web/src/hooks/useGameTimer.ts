import type { GameTimer, TimerConfig, TimerState } from "@/types/timer";
import { getOtherPlayer } from "@/types/timer";
import { useCallback, useEffect, useRef, useState } from "react";
import type { Player } from "shogi-core";

interface UseGameTimerOptions {
    /** Initial timer configuration */
    config: TimerConfig;
    /** Callback when a player times out */
    onTimeout?: (player: Player) => void;
    /** Callback when timer state changes */
    onStateChange?: (state: TimerState) => void;
}

export function useGameTimer({ config, onTimeout, onStateChange }: UseGameTimerOptions): GameTimer {
    const [state, setState] = useState<TimerState>(() => ({
        blackMainTime: config.mainTimeMs,
        whiteMainTime: config.mainTimeMs,
        byoyomiTime: null,
        isRunning: false,
        activePlayer: null,
        hasTimedOut: false,
        timedOutPlayer: null,
    }));

    const [timerConfig, setTimerConfig] = useState<TimerConfig>(config);
    const intervalRef = useRef<NodeJS.Timeout | undefined>(undefined);
    const lastUpdateRef = useRef<number | undefined>(undefined);

    // Clear interval on unmount
    useEffect(() => {
        return () => {
            if (intervalRef.current) {
                clearInterval(intervalRef.current);
            }
        };
    }, []);

    // Notify state changes
    useEffect(() => {
        onStateChange?.(state);
    }, [state, onStateChange]);

    // Timer tick logic
    const tick = useCallback(() => {
        const now = Date.now();
        const deltaMs = lastUpdateRef.current ? now - lastUpdateRef.current : 0;
        lastUpdateRef.current = now;

        setState((prevState) => {
            if (!prevState.isRunning || !prevState.activePlayer || deltaMs <= 0) {
                return prevState;
            }

            const activePlayerKey = `${prevState.activePlayer}MainTime` as keyof Pick<
                TimerState,
                "blackMainTime" | "whiteMainTime"
            >;

            let newState = { ...prevState };

            // If in byoyomi mode
            if (prevState.byoyomiTime !== null) {
                const newByoyomiTime = Math.max(0, prevState.byoyomiTime - deltaMs);

                if (newByoyomiTime <= 0) {
                    // Byoyomi timeout
                    newState = {
                        ...newState,
                        byoyomiTime: 0,
                        isRunning: false,
                        hasTimedOut: true,
                        timedOutPlayer: prevState.activePlayer,
                    };

                    // Call timeout callback asynchronously
                    if (prevState.activePlayer) {
                        const player = prevState.activePlayer;
                        setTimeout(() => onTimeout?.(player), 0);
                    }
                } else {
                    newState.byoyomiTime = newByoyomiTime;
                }
            } else {
                // In main time
                const currentMainTime = prevState[activePlayerKey];
                const newMainTime = Math.max(0, currentMainTime - deltaMs);

                if (newMainTime <= 0) {
                    // Main time exhausted, enter byoyomi if available
                    if (timerConfig.byoyomiMs > 0) {
                        newState = {
                            ...newState,
                            [activePlayerKey]: 0,
                            byoyomiTime: timerConfig.byoyomiMs,
                        };
                    } else {
                        // No byoyomi, timeout
                        newState = {
                            ...newState,
                            [activePlayerKey]: 0,
                            isRunning: false,
                            hasTimedOut: true,
                            timedOutPlayer: prevState.activePlayer,
                        };

                        // Call timeout callback asynchronously
                        if (prevState.activePlayer) {
                            const player = prevState.activePlayer;
                            setTimeout(() => onTimeout?.(player), 0);
                        }
                    }
                } else {
                    newState[activePlayerKey] = newMainTime;
                }
            }

            return newState;
        });
    }, [timerConfig.byoyomiMs, onTimeout]);

    // Start/stop interval based on running state
    useEffect(() => {
        if (state.isRunning && timerConfig.enabled) {
            lastUpdateRef.current = Date.now();
            intervalRef.current = setInterval(tick, 100); // Update every 100ms
        } else {
            if (intervalRef.current) {
                clearInterval(intervalRef.current);
                intervalRef.current = undefined;
            }
            lastUpdateRef.current = undefined;
        }

        return () => {
            if (intervalRef.current) {
                clearInterval(intervalRef.current);
            }
        };
    }, [state.isRunning, timerConfig.enabled, tick]);

    const start = useCallback(
        (player: Player) => {
            if (!timerConfig.enabled) return;

            setState((prev) => ({
                ...prev,
                isRunning: true,
                activePlayer: player,
            }));
        },
        [timerConfig.enabled],
    );

    const pause = useCallback(() => {
        setState((prev) => ({
            ...prev,
            isRunning: false,
        }));
    }, []);

    const resume = useCallback(() => {
        if (!timerConfig.enabled) return;

        setState((prev) => ({
            ...prev,
            isRunning: true,
        }));
    }, [timerConfig.enabled]);

    const switchPlayer = useCallback(() => {
        setState((prev) => {
            if (!prev.activePlayer) return prev;

            return {
                ...prev,
                activePlayer: getOtherPlayer(prev.activePlayer),
                byoyomiTime: null, // Reset byoyomi when switching players
            };
        });
    }, []);

    const reset = useCallback(() => {
        setState({
            blackMainTime: timerConfig.mainTimeMs,
            whiteMainTime: timerConfig.mainTimeMs,
            byoyomiTime: null,
            isRunning: false,
            activePlayer: null,
            hasTimedOut: false,
            timedOutPlayer: null,
        });
    }, [timerConfig.mainTimeMs]);

    const updateConfig = useCallback((newConfig: Partial<TimerConfig>) => {
        setTimerConfig((prev) => ({ ...prev, ...newConfig }));

        // If main time changed, update timer state
        if (newConfig.mainTimeMs !== undefined) {
            const newMainTime = newConfig.mainTimeMs;
            setState((prev) => ({
                ...prev,
                blackMainTime:
                    prev.blackMainTime === prev.whiteMainTime ? newMainTime : prev.blackMainTime,
                whiteMainTime:
                    prev.whiteMainTime === prev.blackMainTime ? newMainTime : prev.whiteMainTime,
            }));
        }
    }, []);

    // Update config when prop changes
    useEffect(() => {
        setTimerConfig(config);
    }, [config]);

    return {
        ...state,
        start,
        pause,
        resume,
        switchPlayer,
        reset,
        updateConfig,
    };
}
