/**
 * 音声システム専用のエラー型定義（function-based）
 */

import type { SoundType } from "./audio";

/**
 * 音声エラーの基本型
 */
export interface AudioError {
    readonly type: "AudioError";
    readonly code: AudioErrorCode;
    readonly message: string;
    readonly recoverable: boolean;
    readonly context?: Record<string, unknown>;
    readonly cause?: Error;
    readonly timestamp: number;
}

/**
 * 音声エラーコード
 */
export type AudioErrorCode =
    | "AUDIO_INIT_FAILED"
    | "AUDIO_GENERATION_FAILED"
    | "AUDIO_PLAYBACK_FAILED"
    | "AUDIO_NOT_SUPPORTED"
    | "AUDIO_POOL_ERROR"
    | "AUDIO_SETTINGS_ERROR";

/**
 * エラー作成ファクトリー関数
 */
const createAudioError = (
    code: AudioErrorCode,
    message: string,
    recoverable: boolean,
    context?: Record<string, unknown>,
    cause?: Error,
): AudioError => ({
    type: "AudioError" as const,
    code,
    message,
    recoverable,
    context,
    cause,
    timestamp: Date.now(),
});

/**
 * 音声エラー作成ヘルパー関数
 */
export const AudioErrors = {
    /**
     * 音声初期化エラー
     */
    initialization: (message: string, cause?: Error): AudioError =>
        createAudioError(
            "AUDIO_INIT_FAILED",
            `Audio initialization failed: ${message}`,
            true,
            undefined,
            cause,
        ),

    /**
     * 音声生成エラー
     */
    generation: (soundType: SoundType, message: string, cause?: Error): AudioError =>
        createAudioError(
            "AUDIO_GENERATION_FAILED",
            `Failed to generate ${soundType} sound: ${message}`,
            true,
            { soundType },
            cause,
        ),

    /**
     * 音声再生エラー
     */
    playback: (soundType: SoundType, message: string, cause?: Error): AudioError =>
        createAudioError(
            "AUDIO_PLAYBACK_FAILED",
            `Failed to play ${soundType} sound: ${message}`,
            true,
            { soundType },
            cause,
        ),

    /**
     * サポートされていない機能エラー
     */
    notSupported: (feature: string): AudioError =>
        createAudioError("AUDIO_NOT_SUPPORTED", `Audio feature not supported: ${feature}`, false, {
            feature,
        }),

    /**
     * プールエラー
     */
    pool: (message: string, context?: Record<string, unknown>): AudioError =>
        createAudioError("AUDIO_POOL_ERROR", message, true, context),

    /**
     * 設定エラー
     */
    settings: (message: string, context?: Record<string, unknown>): AudioError =>
        createAudioError("AUDIO_SETTINGS_ERROR", message, true, context),
} as const;

/**
 * エラー判定ヘルパー
 */
export const isAudioError = (error: unknown): error is AudioError =>
    typeof error === "object" && error !== null && "type" in error && error.type === "AudioError";

/**
 * エラーコード判定ヘルパー
 */
export const hasErrorCode = (error: AudioError, code: AudioErrorCode): boolean =>
    error.code === code;

/**
 * リカバリー可能エラー判定
 */
export const isRecoverable = (error: AudioError): boolean => error.recoverable;
