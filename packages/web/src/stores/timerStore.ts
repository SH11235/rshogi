import type { TimerConfig, TimerState } from "@/types/timer";
import { getOtherPlayer, minutesToMs, secondsToMs } from "@/types/timer";
import type { Player } from "shogi-core";
import { create } from "zustand";

interface TimerStore extends TimerState {
    config: TimerConfig;

    // Actions
    updateConfig: (config: Partial<TimerConfig>) => void;
    start: (player: Player) => void;
    pause: () => void;
    resume: () => void;
    switchPlayer: () => void;
    reset: () => void;
    handleTimeout: (player: Player) => void;

    // Internal
    tick: () => void;
    _lastUpdate: number | null;
}

export const useTimerStore = create<TimerStore>((set, get) => ({
    // Initial state
    blackMainTime: minutesToMs(10), // Default 10 minutes
    whiteMainTime: minutesToMs(10),
    byoyomiTime: null,
    isRunning: false,
    activePlayer: null,
    hasTimedOut: false,
    timedOutPlayer: null,

    config: {
        mainTimeMs: minutesToMs(10),
        byoyomiMs: secondsToMs(30),
        enabled: false,
    },

    _lastUpdate: null,

    updateConfig: (newConfig: Partial<TimerConfig>) => {
        const { config } = get();
        const updatedConfig = { ...config, ...newConfig };

        set((state) => ({
            config: updatedConfig,
            // Reset times when main time config changes
            blackMainTime:
                newConfig.mainTimeMs !== undefined ? newConfig.mainTimeMs : state.blackMainTime,
            whiteMainTime:
                newConfig.mainTimeMs !== undefined ? newConfig.mainTimeMs : state.whiteMainTime,
        }));
    },

    start: (player: Player) => {
        const { config } = get();
        if (!config.enabled) return;

        set({
            isRunning: true,
            activePlayer: player,
            _lastUpdate: Date.now(),
        });
    },

    pause: () => {
        set({
            isRunning: false,
            _lastUpdate: null,
        });
    },

    resume: () => {
        const { config } = get();
        if (!config.enabled) return;

        set({
            isRunning: true,
            _lastUpdate: Date.now(),
        });
    },

    switchPlayer: () => {
        const { activePlayer, config } = get();
        if (!activePlayer || !config.enabled) return;

        set({
            activePlayer: getOtherPlayer(activePlayer),
            byoyomiTime: null, // Reset byoyomi when switching
            _lastUpdate: Date.now(),
        });
    },

    reset: () => {
        const { config } = get();
        set({
            blackMainTime: config.mainTimeMs,
            whiteMainTime: config.mainTimeMs,
            byoyomiTime: null,
            isRunning: false,
            activePlayer: null,
            hasTimedOut: false,
            timedOutPlayer: null,
            _lastUpdate: null,
        });
    },

    handleTimeout: (player: Player) => {
        set({
            isRunning: false,
            hasTimedOut: true,
            timedOutPlayer: player,
            _lastUpdate: null,
        });
    },

    tick: () => {
        const state = get();
        const {
            isRunning,
            activePlayer,
            blackMainTime,
            whiteMainTime,
            byoyomiTime,
            config,
            _lastUpdate,
        } = state;

        if (!isRunning || !activePlayer || !_lastUpdate || !config.enabled) {
            return;
        }

        const now = Date.now();
        const deltaMs = now - _lastUpdate;

        if (deltaMs <= 0) return;

        const activePlayerMainTime = activePlayer === "black" ? blackMainTime : whiteMainTime;

        if (byoyomiTime !== null) {
            // In byoyomi mode
            const newByoyomiTime = Math.max(0, byoyomiTime - deltaMs);

            if (newByoyomiTime <= 0) {
                // Byoyomi timeout
                get().handleTimeout(activePlayer);
            } else {
                set({
                    byoyomiTime: newByoyomiTime,
                    _lastUpdate: now,
                });
            }
        } else {
            // In main time
            const newMainTime = Math.max(0, activePlayerMainTime - deltaMs);

            if (newMainTime <= 0) {
                if (config.byoyomiMs > 0) {
                    // Enter byoyomi
                    set({
                        [activePlayer === "black" ? "blackMainTime" : "whiteMainTime"]: 0,
                        byoyomiTime: config.byoyomiMs,
                        _lastUpdate: now,
                    });
                } else {
                    // No byoyomi, timeout
                    set({
                        [activePlayer === "black" ? "blackMainTime" : "whiteMainTime"]: 0,
                    });
                    get().handleTimeout(activePlayer);
                }
            } else {
                set({
                    [activePlayer === "black" ? "blackMainTime" : "whiteMainTime"]: newMainTime,
                    _lastUpdate: now,
                });
            }
        }
    },
}));

// Timer interval management
let timerInterval: NodeJS.Timeout | null = null;

export function startTimerInterval() {
    if (timerInterval) return;

    timerInterval = setInterval(() => {
        useTimerStore.getState().tick();
    }, 100); // Update every 100ms for smooth countdown
}

export function stopTimerInterval() {
    if (timerInterval) {
        clearInterval(timerInterval);
        timerInterval = null;
    }
}

// Auto-start interval when store is first used
startTimerInterval();
