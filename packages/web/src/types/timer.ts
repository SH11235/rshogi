import type { Player } from "shogi-core";

export interface TimerState {
    /** Main time remaining for black player in milliseconds */
    blackMainTime: number;
    /** Main time remaining for white player in milliseconds */
    whiteMainTime: number;
    /** Current byoyomi countdown in milliseconds (null if not in byoyomi) */
    byoyomiTime: number | null;
    /** Whether the timer is currently running */
    isRunning: boolean;
    /** Which player's timer is currently active */
    activePlayer: Player | null;
    /** Whether game has ended due to timeout */
    hasTimedOut: boolean;
    /** Player who timed out (if any) */
    timedOutPlayer: Player | null;
}

export interface TimerConfig {
    /** Main time per player in milliseconds */
    mainTimeMs: number;
    /** Byoyomi time in milliseconds (0 means no byoyomi) */
    byoyomiMs: number;
    /** Whether timer is enabled */
    enabled: boolean;
}

export interface TimerActions {
    /** Start the timer for a specific player */
    start: (player: Player) => void;
    /** Pause the timer */
    pause: () => void;
    /** Resume the timer */
    resume: () => void;
    /** Switch to the other player's timer */
    switchPlayer: () => void;
    /** Reset the timer to initial state */
    reset: () => void;
    /** Update timer configuration */
    updateConfig: (config: Partial<TimerConfig>) => void;
}

export interface GameTimer extends TimerState, TimerActions {}

/** Convert minutes to milliseconds */
export function minutesToMs(minutes: number): number {
    return minutes * 60 * 1000;
}

/** Convert seconds to milliseconds */
export function secondsToMs(seconds: number): number {
    return seconds * 1000;
}

/** Convert milliseconds to readable time format */
export function formatTime(ms: number): string {
    if (ms <= 0) return "0:00";

    const totalSeconds = Math.ceil(ms / 1000);
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;

    return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

/** Get the other player */
export function getOtherPlayer(player: Player): Player {
    return player === "black" ? "white" : "black";
}
