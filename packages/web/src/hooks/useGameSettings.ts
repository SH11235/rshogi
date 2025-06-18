import {
    type AudioSettings,
    type DisplaySettings,
    type GameSettings,
    type TimeControlSettings,
    defaultGameSettings,
} from "@/types/settings";
import { useCallback } from "react";
import { useLocalStorage } from "./useLocalStorage";

const SETTINGS_STORAGE_KEY = "shogi-game-settings";

/**
 * ゲーム設定を管理する hook
 */
export function useGameSettings() {
    const [settings, setSettings, removeSettings] = useLocalStorage<GameSettings>(
        SETTINGS_STORAGE_KEY,
        defaultGameSettings,
    );

    // 時間制御設定の更新
    const updateTimeControl = useCallback(
        (timeControl: Partial<TimeControlSettings>) => {
            setSettings((prev) => ({
                ...prev,
                timeControl: { ...prev.timeControl, ...timeControl },
            }));
        },
        [setSettings],
    );

    // 音声設定の更新
    const updateAudio = useCallback(
        (audio: Partial<AudioSettings>) => {
            setSettings((prev) => ({
                ...prev,
                audio: { ...prev.audio, ...audio },
            }));
        },
        [setSettings],
    );

    // 表示設定の更新
    const updateDisplay = useCallback(
        (display: Partial<DisplaySettings>) => {
            setSettings((prev) => ({
                ...prev,
                display: { ...prev.display, ...display },
            }));
        },
        [setSettings],
    );

    // 設定の完全更新
    const updateSettings = useCallback(
        (newSettings: Partial<GameSettings>) => {
            setSettings((prev) => ({ ...prev, ...newSettings }));
        },
        [setSettings],
    );

    // 設定のリセット
    const resetSettings = useCallback(() => {
        setSettings(defaultGameSettings);
    }, [setSettings]);

    // 設定の削除（localStorage から完全削除）
    const clearSettings = useCallback(() => {
        removeSettings();
    }, [removeSettings]);

    // 設定の検証と修正
    const validateAndFixSettings = useCallback(
        (settingsToValidate: Partial<GameSettings>): GameSettings => {
            const validated: GameSettings = {
                timeControl: {
                    mainTimeMinutes:
                        settingsToValidate.timeControl?.mainTimeMinutes ??
                        defaultGameSettings.timeControl.mainTimeMinutes,
                    byoyomiSeconds:
                        settingsToValidate.timeControl?.byoyomiSeconds ??
                        defaultGameSettings.timeControl.byoyomiSeconds,
                    enabled:
                        settingsToValidate.timeControl?.enabled ??
                        defaultGameSettings.timeControl.enabled,
                },
                audio: {
                    masterVolume:
                        settingsToValidate.audio?.masterVolume ??
                        defaultGameSettings.audio.masterVolume,
                    pieceSound:
                        settingsToValidate.audio?.pieceSound ??
                        defaultGameSettings.audio.pieceSound,
                    checkSound:
                        settingsToValidate.audio?.checkSound ??
                        defaultGameSettings.audio.checkSound,
                    gameEndSound:
                        settingsToValidate.audio?.gameEndSound ??
                        defaultGameSettings.audio.gameEndSound,
                },
                display: {
                    theme: settingsToValidate.display?.theme ?? defaultGameSettings.display.theme,
                    animations:
                        settingsToValidate.display?.animations ??
                        defaultGameSettings.display.animations,
                    showValidMoves:
                        settingsToValidate.display?.showValidMoves ??
                        defaultGameSettings.display.showValidMoves,
                    showLastMove:
                        settingsToValidate.display?.showLastMove ??
                        defaultGameSettings.display.showLastMove,
                },
            };

            // 音量は 0-100 の範囲に制限
            if (validated.audio.masterVolume < 0 || validated.audio.masterVolume > 100) {
                validated.audio.masterVolume = defaultGameSettings.audio.masterVolume;
            }

            return validated;
        },
        [],
    );

    return {
        // 現在の設定
        settings,

        // 個別設定の更新
        updateTimeControl,
        updateAudio,
        updateDisplay,

        // 全体設定の更新
        updateSettings,

        // 設定の管理
        resetSettings,
        clearSettings,
        validateAndFixSettings,

        // 便利なゲッター
        isTimeControlEnabled: settings.timeControl.enabled,
        currentTheme: settings.display.theme,
        isSoundEnabled: settings.audio.masterVolume > 0,
    };
}
