// Re-export timer types from core
import type { Player, TimerConfig, TimerState } from "shogi-core";

export type { Player } from "shogi-core";
export {
    type TimerMode,
    type WarningLevel,
    type TimerConfig,
    type TimerState,
    type TimerPresetKey,
    TIMER_PRESETS,
    minutesToMs,
    secondsToMs,
    formatTime,
    formatTimeWithHours,
    formatByoyomiTime,
    getWarningLevel,
    getOtherPlayer,
} from "shogi-core";

export interface TimerActions {
    // 初期設定
    initializeTimer: (config: TimerConfig) => void;

    // タイマー制御
    startPlayerTimer: (player: Player) => void;
    switchTimer: () => void; // 手番交代時の時計切り替え
    pauseTimer: () => void;
    resumeTimer: () => void;
    resetTimer: () => void;

    // 時間更新（内部処理）
    tick: () => void;

    // 警告レベル更新
    updateWarningLevels: () => void;
}

export interface GameTimer extends TimerState, TimerActions {}
