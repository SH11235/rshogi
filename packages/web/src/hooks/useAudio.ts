import { audioManager } from "@/services/audioManager";
import type { AudioPlayerState, PlayOptions, SoundType } from "@/types/audio";
import { useCallback, useEffect, useState } from "react";
import { useGameSettings } from "./useGameSettings";

interface UseAudioReturn {
    /** 音声を再生 */
    play: (type: SoundType, options?: PlayOptions) => Promise<void>;
    /** 設定を無視して音声を強制再生（テスト用） */
    playForced: (type: SoundType, options?: PlayOptions) => Promise<void>;
    /** 音声システムの状態 */
    state: AudioPlayerState;
    /** 初期化済みかどうか */
    isReady: boolean;
    /** 音声を初期化（手動） */
    initialize: () => Promise<void>;
}

/**
 * 音声再生のためのReact hook
 */
export function useAudio(): UseAudioReturn {
    const { settings } = useGameSettings();
    const [state, setState] = useState<AudioPlayerState>(audioManager.getState());
    const [isReady, setIsReady] = useState(false);

    // 設定変更時に音量とミュート状態を更新
    useEffect(() => {
        const { audio } = settings;
        audioManager.setVolume(audio.masterVolume);
        audioManager.setMuted(audio.masterVolume === 0);
    }, [settings]);

    // 初期化処理
    const initialize = useCallback(async () => {
        try {
            await audioManager.initialize();
            setIsReady(true);
            setState(audioManager.getState());
        } catch (error) {
            console.warn("Audio initialization failed:", error);
        }
    }, []);

    // 初回マウント時に自動初期化
    useEffect(() => {
        initialize();
    }, [initialize]);

    // 再生関数
    const play = useCallback(
        async (type: SoundType, options?: PlayOptions) => {
            if (!isReady) {
                return;
            }

            const { audio } = settings;

            // 個別の音声設定をチェック
            const isEnabled = (() => {
                switch (type) {
                    case "piece":
                        return audio.pieceSound;
                    case "check":
                        return audio.checkSound;
                    case "gameEnd":
                        return audio.gameEndSound;
                    default:
                        return false;
                }
            })();

            if (!isEnabled || audio.masterVolume === 0) {
                return;
            }

            try {
                await audioManager.play(type, options);
                setState(audioManager.getState());
            } catch (error) {
                console.debug(`Failed to play ${type}:`, error);
            }
        },
        [isReady, settings],
    );

    // 状態更新を定期的にチェック（オプショナル）
    useEffect(() => {
        const interval = setInterval(() => {
            setState(audioManager.getState());
        }, 1000);

        return () => clearInterval(interval);
    }, []);

    // 設定を無視して強制再生する関数（テスト用）
    const playForced = useCallback(
        async (type: SoundType, options?: PlayOptions) => {
            console.log(`useAudio: playForced called for ${type}, isReady=${isReady}`);

            if (!isReady) {
                console.log("useAudio: Audio not ready, skipping playForced");
                return;
            }

            try {
                console.log(`useAudio: Calling audioManager.play for ${type}`);
                await audioManager.play(type, options);
                setState(audioManager.getState());
                console.log(`useAudio: Successfully played ${type}`);
            } catch (error) {
                console.error(`useAudio: Failed to play ${type}:`, error);
            }
        },
        [isReady],
    );

    return {
        play,
        playForced,
        state,
        isReady,
        initialize,
    };
}

/**
 * ユーザーインタラクション時に音声を初期化するためのhook
 */
export function useAudioInitializer() {
    const [hasUserInteracted, setHasUserInteracted] = useState(false);
    const { initialize } = useAudio();

    const initializeOnInteraction = useCallback(async () => {
        if (!hasUserInteracted) {
            setHasUserInteracted(true);
            await initialize();
        }
    }, [hasUserInteracted, initialize]);

    // グローバルクリックイベントで初期化
    useEffect(() => {
        if (hasUserInteracted) return;

        const handleInteraction = () => {
            initializeOnInteraction();
        };

        document.addEventListener("click", handleInteraction, { once: true });
        document.addEventListener("keydown", handleInteraction, { once: true });
        document.addEventListener("touchstart", handleInteraction, { once: true });

        return () => {
            document.removeEventListener("click", handleInteraction);
            document.removeEventListener("keydown", handleInteraction);
            document.removeEventListener("touchstart", handleInteraction);
        };
    }, [hasUserInteracted, initializeOnInteraction]);

    return {
        hasUserInteracted,
        initializeOnInteraction,
    };
}
