import { useEffect } from "react";

interface KeyboardShortcutsConfig {
    onUndo?: () => void;
    onRedo?: () => void;
    onReset?: () => void;
    onEscape?: () => void;
    onEnter?: () => void;
}

/**
 * キーボードショートカット機能を提供するhook
 * @param config ショートカットの設定とコールバック関数
 */
export function useKeyboardShortcuts(config: KeyboardShortcutsConfig) {
    useEffect(() => {
        const handleKeyDown = (event: KeyboardEvent) => {
            // フォーカスがinput/textareaの場合はショートカットを無効化
            const activeElement = document.activeElement;
            if (
                activeElement &&
                (activeElement.tagName === "INPUT" ||
                    activeElement.tagName === "TEXTAREA" ||
                    activeElement.getAttribute("contenteditable") === "true")
            ) {
                return;
            }

            // Ctrl+Z: 元に戻す
            if (event.ctrlKey && event.key === "z" && !event.shiftKey && config.onUndo) {
                event.preventDefault();
                config.onUndo();
                return;
            }

            // Ctrl+Y または Ctrl+Shift+Z: やり直し
            if (
                ((event.ctrlKey && event.key === "y") ||
                    (event.ctrlKey && event.shiftKey && event.key === "Z")) &&
                config.onRedo
            ) {
                event.preventDefault();
                config.onRedo();
                return;
            }

            // R: リセット
            if (event.key === "r" && !event.ctrlKey && !event.altKey && config.onReset) {
                event.preventDefault();
                config.onReset();
                return;
            }

            // Escape: 選択解除・ダイアログクローズ
            if (event.key === "Escape" && config.onEscape) {
                event.preventDefault();
                config.onEscape();
                return;
            }

            // Enter: プロモーションダイアログで成る
            if (event.key === "Enter" && config.onEnter) {
                event.preventDefault();
                config.onEnter();
                return;
            }
        };

        document.addEventListener("keydown", handleKeyDown);

        return () => {
            document.removeEventListener("keydown", handleKeyDown);
        };
    }, [config]);
}
