import type { Player } from "../../domain/model/piece";

export type TimerMode = "basic" | "fischer" | "perMove" | null;
export type WarningLevel = "normal" | "low" | "critical" | "byoyomi";

export interface TimerConfig {
    mode: TimerMode;
    basicTime: number; // 基本時間（秒）
    byoyomiTime: number; // 秒読み時間（秒）
    fischerIncrement: number; // フィッシャー加算時間（秒）
    perMoveLimit: number; // 一手制限時間（秒）
}

export interface TimerState {
    config: TimerConfig;

    // 各プレイヤーの時間状態
    blackTime: number; // 先手残り時間（ミリ秒）
    whiteTime: number; // 後手残り時間（ミリ秒）
    blackInByoyomi: boolean; // 先手が秒読み中
    whiteInByoyomi: boolean; // 後手が秒読み中

    // 制御状態
    activePlayer: Player | null;
    isPaused: boolean;
    lastTickTime: number; // 最後の更新時刻（Date.now()）

    // 警告レベル
    blackWarningLevel: WarningLevel;
    whiteWarningLevel: WarningLevel;

    // 時間切れ状態
    hasTimedOut: boolean;
    timedOutPlayer: Player | null;
}

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

/** Format time for display with proper handling for hours */
export function formatTimeWithHours(ms: number): string {
    if (ms <= 0) return "0:00:00";

    const totalSeconds = Math.floor(ms / 1000);
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = totalSeconds % 60;

    if (hours > 0) {
        return `${hours}:${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
    }
    return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

/** Format byoyomi time with 1 decimal place */
export function formatByoyomiTime(ms: number): string {
    const seconds = ms / 1000;
    return `秒読み ${seconds.toFixed(1)}秒`;
}

/** Get warning level based on remaining time */
export function getWarningLevel(time: number, inByoyomi: boolean): WarningLevel {
    if (inByoyomi) return "byoyomi";
    if (time <= 60000) return "critical"; // 1分以下
    if (time <= 300000) return "low"; // 5分以下
    return "normal";
}

// タイマー設定のプリセット
export const TIMER_PRESETS = {
    rapid10: {
        mode: "basic" as TimerMode,
        basicTime: 600, // 10分
        byoyomiTime: 30,
        fischerIncrement: 0,
        perMoveLimit: 0,
    },
    normal30: {
        mode: "basic" as TimerMode,
        basicTime: 1800, // 30分
        byoyomiTime: 60,
        fischerIncrement: 0,
        perMoveLimit: 0,
    },
    long60: {
        mode: "basic" as TimerMode,
        basicTime: 3600, // 1時間
        byoyomiTime: 60,
        fischerIncrement: 0,
        perMoveLimit: 0,
    },
    fischer10plus30: {
        mode: "fischer" as TimerMode,
        basicTime: 600, // 10分
        byoyomiTime: 0,
        fischerIncrement: 30,
        perMoveLimit: 0,
    },
    perMove1min: {
        mode: "perMove" as TimerMode,
        basicTime: 0,
        byoyomiTime: 0,
        fischerIncrement: 0,
        perMoveLimit: 60,
    },
} as const;

export type TimerPresetKey = keyof typeof TIMER_PRESETS;
