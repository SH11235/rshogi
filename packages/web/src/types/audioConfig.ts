/**
 * 音声システムの設定管理
 */

import type { SoundType } from "./audio";
import type { VolumeLevel } from "./settings";

export interface AudioEnvironmentConfig {
    /** 開発環境かどうか */
    isDevelopment: boolean;
    /** 実際の音声ファイルを使用するか（falseの場合は生成音声を使用） */
    useRealAudioFiles: boolean;
    /** デバッグログを有効にするか */
    enableDebugLogs: boolean;
    /** 同時再生可能な音声数 */
    maxConcurrentSounds: number;
}

export type SoundGenerationConfig = {
    /** 音声タイプごとの生成パラメータ */
    [K in SoundType]: {
        frequency: number;
        duration: number;
        volume: number;
    };
};

export interface RuntimeAudioSettings {
    /** マスター音量 */
    masterVolume: VolumeLevel;
    /** ミュート状態 */
    isMuted: boolean;
    /** 個別音声の有効/無効設定 */
    enabledSounds: Record<SoundType, boolean>;
}

/**
 * デフォルト設定
 */
export const defaultAudioEnvironmentConfig: AudioEnvironmentConfig = {
    isDevelopment: import.meta.env.DEV,
    useRealAudioFiles: false, // 開発環境では生成音声を使用
    enableDebugLogs: import.meta.env.DEV,
    maxConcurrentSounds: 3,
};

export const defaultSoundGenerationConfig: SoundGenerationConfig = {
    piece: { frequency: 800, duration: 0.1, volume: 0.2 },
    check: { frequency: 600, duration: 0.3, volume: 0.4 },
    gameEnd: { frequency: 400, duration: 0.5, volume: 0.3 },
};

/**
 * ゲーム設定から実行時音声設定に変換
 */
export function createRuntimeAudioSettings(gameSettings: {
    audio: {
        masterVolume: VolumeLevel;
        pieceSound: boolean;
        checkSound: boolean;
        gameEndSound: boolean;
    };
}): RuntimeAudioSettings {
    return {
        masterVolume: gameSettings.audio.masterVolume,
        isMuted: gameSettings.audio.masterVolume === 0,
        enabledSounds: {
            piece: gameSettings.audio.pieceSound,
            check: gameSettings.audio.checkSound,
            gameEnd: gameSettings.audio.gameEndSound,
        },
    };
}
