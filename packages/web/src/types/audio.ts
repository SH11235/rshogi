import type { VolumeLevel } from "./settings";

/**
 * 音声ファイルの種類
 */
export type SoundType = "piece" | "check" | "gameEnd";

/**
 * 音声ファイル設定
 */
export interface SoundConfig {
    /** ファイルパス */
    path: string;
    /** デフォルト音量（0-100） */
    defaultVolume?: VolumeLevel;
    /** プリロードするか */
    preload?: boolean;
}

/**
 * 音声ファイルマップ
 */
export interface SoundMap {
    piece: SoundConfig;
    check: SoundConfig;
    gameEnd: SoundConfig;
}

/**
 * オーディオプレイヤーの状態
 */
export interface AudioPlayerState {
    /** 音声が初期化されているか */
    isInitialized: boolean;
    /** 現在の音量レベル */
    volume: VolumeLevel;
    /** ミュート状態 */
    isMuted: boolean;
    /** 読み込み済みの音声ファイル */
    loadedSounds: Set<SoundType>;
}

/**
 * 再生オプション
 */
export interface PlayOptions {
    /** 音量を上書き（0-1の範囲） */
    volume?: number;
    /** 再生速度 */
    playbackRate?: number;
    /** 現在再生中の同じ音声を停止するか */
    interrupt?: boolean;
}

/**
 * オーディオマネージャーのインターフェース
 */
export interface AudioManager {
    /** 音声を初期化 */
    initialize: () => Promise<void>;
    /** 音声を再生 */
    play: (type: SoundType, options?: PlayOptions) => Promise<void>;
    /** 音量を設定 */
    setVolume: (volume: VolumeLevel) => void;
    /** ミュート/ミュート解除 */
    setMuted: (muted: boolean) => void;
    /** 音声をプリロード */
    preload: (type: SoundType) => Promise<void>;
    /** すべての音声を停止 */
    stopAll: () => void;
    /** 現在の状態を取得 */
    getState: () => AudioPlayerState;
}

/**
 * 音量レベルを0-1の範囲に変換
 */
export function volumeLevelToFloat(volume: VolumeLevel): number {
    return volume / 100;
}

/**
 * 0-1の範囲の音量をVolumeLevel（0-100）に変換
 */
export function floatToVolumeLevel(volume: number): VolumeLevel {
    const level = Math.round(Math.max(0, Math.min(1, volume)) * 100);
    return level as VolumeLevel;
}
