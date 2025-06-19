/**
 * ゲーム設定の型定義
 */

/**
 * 持ち時間設定（分単位）
 */
export type TimeControlMinutes = 1 | 3 | 5 | 10 | 15 | 30 | 60 | 90;

/**
 * 秒読み時間設定（秒単位）
 */
export type ByoyomiSeconds = 0 | 10 | 30 | 60;

/**
 * 音量レベル
 */
export type VolumeLevel = 0 | 25 | 50 | 75 | 100;

/**
 * テーマ設定
 */
export type Theme = "light" | "dark" | "auto";

/**
 * 時間制御設定
 */
export interface TimeControlSettings {
    /** 持ち時間（分） */
    mainTimeMinutes: TimeControlMinutes;
    /** 秒読み時間（秒） */
    byoyomiSeconds: ByoyomiSeconds;
    /** 時間制御を有効にするか */
    enabled: boolean;
}

/**
 * 音声設定
 */
export interface AudioSettings {
    /** マスターボリューム */
    masterVolume: VolumeLevel;
    /** 駒音を有効にするか */
    pieceSound: boolean;
    /** 王手音を有効にするか */
    checkSound: boolean;
    /** ゲーム終了音を有効にするか */
    gameEndSound: boolean;
}

/**
 * 表示設定
 */
export interface DisplaySettings {
    /** テーマ */
    theme: Theme;
    /** アニメーションを有効にするか */
    animations: boolean;
    /** 有効手のハイライトを表示するか */
    showValidMoves: boolean;
    /** 最後の手をハイライトするか */
    showLastMove: boolean;
}

/**
 * ゲーム設定
 */
export interface GameSettings {
    /** 時間制御設定 */
    timeControl: TimeControlSettings;
    /** 音声設定 */
    audio: AudioSettings;
    /** 表示設定 */
    display: DisplaySettings;
}

/**
 * デフォルトのゲーム設定
 */
export const defaultGameSettings: GameSettings = {
    timeControl: {
        mainTimeMinutes: 10,
        byoyomiSeconds: 30,
        enabled: false,
    },
    audio: {
        masterVolume: 50,
        pieceSound: true,
        checkSound: true,
        gameEndSound: true,
    },
    display: {
        theme: "auto",
        animations: true,
        showValidMoves: true,
        showLastMove: true,
    },
};
