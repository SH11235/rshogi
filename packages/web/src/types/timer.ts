// Timer mode types
export type TimerMode = "basic" | "fischer" | "perMove";

// Timer configuration
export interface TimerConfig {
    mode: TimerMode | null;
    basicTime: number; // seconds
    byoyomiTime: number; // seconds
    fischerIncrement: number; // seconds
    perMoveLimit: number; // seconds
}

// Timer preset keys
export type TimerPresetKey = "rapid10" | "normal30" | "fischer10plus30" | "perMove1min" | "custom";

// Timer presets
export const TIMER_PRESETS: Record<TimerPresetKey, TimerConfig> = {
    rapid10: {
        mode: "basic",
        basicTime: 600, // 10 minutes
        byoyomiTime: 30, // 30 seconds
        fischerIncrement: 0,
        perMoveLimit: 0,
    },
    normal30: {
        mode: "basic",
        basicTime: 1800, // 30 minutes
        byoyomiTime: 60, // 60 seconds
        fischerIncrement: 0,
        perMoveLimit: 0,
    },
    fischer10plus30: {
        mode: "fischer",
        basicTime: 600, // 10 minutes
        byoyomiTime: 0,
        fischerIncrement: 30, // 30 seconds
        perMoveLimit: 0,
    },
    perMove1min: {
        mode: "perMove",
        basicTime: 0,
        byoyomiTime: 0,
        fischerIncrement: 0,
        perMoveLimit: 60, // 1 minute
    },
    custom: {
        mode: null,
        basicTime: 0,
        byoyomiTime: 0,
        fischerIncrement: 0,
        perMoveLimit: 0,
    },
};

// Timer state
export type WarningLevel = "normal" | "warning" | "critical";

export interface TimerState {
    config: TimerConfig;
    blackTime: number; // milliseconds
    whiteTime: number; // milliseconds
    blackInByoyomi: boolean;
    whiteInByoyomi: boolean;
    activePlayer: "black" | "white" | null;
    isPaused: boolean;
    lastTickTime: number;
    blackWarningLevel: WarningLevel;
    whiteWarningLevel: WarningLevel;
    hasTimedOut: boolean;
    timedOutPlayer: "black" | "white" | null;
}

// Timer actions
export interface TimerActions {
    initializeTimer: (config: TimerConfig) => void;
    startPlayerTimer: (player: "black" | "white") => void;
    switchTimer: () => void;
    pauseTimer: () => void;
    resumeTimer: () => void;
    resetTimer: () => void;
    tick: () => void;
    updateWarningLevels: () => void;
}

// Helper function to determine warning level
export function getWarningLevel(timeMs: number, inByoyomi: boolean): WarningLevel {
    const seconds = timeMs / 1000;

    if (inByoyomi || seconds <= 10) {
        return "critical";
    }
    if (seconds <= 30) {
        return "warning";
    }
    return "normal";
}

// Format time for display (with hours if needed)
export function formatTimeWithHours(timeMs: number): string {
    const totalSeconds = Math.floor(timeMs / 1000);
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = totalSeconds % 60;

    if (hours > 0) {
        return `${hours}:${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
    }
    return `${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
}

// Format byoyomi time
export function formatByoyomiTime(timeMs: number): string {
    const seconds = Math.floor(timeMs / 1000);
    return `秒読み ${seconds}`;
}
