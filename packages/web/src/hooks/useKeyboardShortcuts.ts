import { useEffect } from "react";

interface KeyboardShortcutsProps {
    onUndo: () => void;
    onRedo: () => void;
    canUndo: boolean;
    canRedo: boolean;
}

/**
 * キーボードショートカットのカスタムhook
 * Ctrl+Z: 戻る
 * Ctrl+Y: 進む
 * Cmd+Z: 戻る (Mac)
 * Cmd+Shift+Z: 進む (Mac)
 */
export function useKeyboardShortcuts({ onUndo, onRedo, canUndo, canRedo }: KeyboardShortcutsProps) {
    useEffect(() => {
        const handleKeyDown = (event: KeyboardEvent) => {
            // フォーカスがinput要素やtextarea要素にある場合は無視
            const target = event.target as HTMLElement;
            if (
                target.tagName === "INPUT" ||
                target.tagName === "TEXTAREA" ||
                target.contentEditable === "true"
            ) {
                return;
            }

            const isCtrlOrCmd = event.ctrlKey || event.metaKey;

            if (isCtrlOrCmd && !event.shiftKey && event.key === "z") {
                // Ctrl+Z または Cmd+Z: 戻る
                event.preventDefault();
                if (canUndo) {
                    onUndo();
                }
            } else if (
                (isCtrlOrCmd && event.key === "y") ||
                (event.metaKey && event.shiftKey && event.key === "z")
            ) {
                // Ctrl+Y または Cmd+Shift+Z: 進む
                event.preventDefault();
                if (canRedo) {
                    onRedo();
                }
            }
        };

        document.addEventListener("keydown", handleKeyDown);

        return () => {
            document.removeEventListener("keydown", handleKeyDown);
        };
    }, [onUndo, onRedo, canUndo, canRedo]);
}
