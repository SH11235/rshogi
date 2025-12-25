/**
 * 棋譜キーボード・ホイールナビゲーションフック
 *
 * キーボードの矢印キーとマウスホイールで棋譜をナビゲート
 * 対局中は無効化される
 */

import { useCallback, useEffect, useRef } from "react";

interface UseKifuKeyboardNavigationOptions {
    /** 1手進む */
    onForward: () => void;
    /** 1手戻る */
    onBack: () => void;
    /** 最初へ */
    onToStart: () => void;
    /** 最後へ */
    onToEnd: () => void;
    /** ナビゲーション無効化（対局中など） */
    disabled?: boolean;
    /** ホイールイベントを受け取るコンテナ要素 */
    containerRef?: React.RefObject<HTMLElement | null>;
}

/**
 * 棋譜のキーボード・ホイールナビゲーションを提供するフック
 *
 * - ←/↑: 1手戻る
 * - →/↓: 1手進む
 * - Home: 開始局面へ
 * - End: 最終局面へ
 * - マウスホイール上: 1手戻る
 * - マウスホイール下: 1手進む
 */
export function useKifuKeyboardNavigation({
    onForward,
    onBack,
    onToStart,
    onToEnd,
    disabled = false,
    containerRef,
}: UseKifuKeyboardNavigationOptions): void {
    // コールバックをrefで保持して最新の値を参照
    const callbacksRef = useRef({ onForward, onBack, onToStart, onToEnd });
    callbacksRef.current = { onForward, onBack, onToStart, onToEnd };

    const disabledRef = useRef(disabled);
    disabledRef.current = disabled;

    // キーボードイベントハンドラ
    const handleKeyDown = useCallback((event: KeyboardEvent) => {
        if (disabledRef.current) return;

        // 入力フィールドにフォーカスがある場合は無視
        const target = event.target as HTMLElement;
        if (
            target.tagName === "INPUT" ||
            target.tagName === "TEXTAREA" ||
            target.isContentEditable
        ) {
            return;
        }

        switch (event.key) {
            case "ArrowLeft":
            case "ArrowUp":
                event.preventDefault();
                callbacksRef.current.onBack();
                break;
            case "ArrowRight":
            case "ArrowDown":
                event.preventDefault();
                callbacksRef.current.onForward();
                break;
            case "Home":
                event.preventDefault();
                callbacksRef.current.onToStart();
                break;
            case "End":
                event.preventDefault();
                callbacksRef.current.onToEnd();
                break;
        }
    }, []);

    // ホイールイベントハンドラ
    const handleWheel = useCallback((event: WheelEvent) => {
        if (disabledRef.current) return;

        // 縦スクロールのみ処理
        if (Math.abs(event.deltaY) < Math.abs(event.deltaX)) return;

        event.preventDefault();

        if (event.deltaY > 0) {
            // 下にスクロール = 1手進む
            callbacksRef.current.onForward();
        } else if (event.deltaY < 0) {
            // 上にスクロール = 1手戻る
            callbacksRef.current.onBack();
        }
    }, []);

    // キーボードイベントの登録（document全体）
    useEffect(() => {
        document.addEventListener("keydown", handleKeyDown);
        return () => {
            document.removeEventListener("keydown", handleKeyDown);
        };
    }, [handleKeyDown]);

    // ホイールイベントの登録（コンテナ要素）
    useEffect(() => {
        const container = containerRef?.current;
        if (!container) return;

        container.addEventListener("wheel", handleWheel, { passive: false });
        return () => {
            container.removeEventListener("wheel", handleWheel);
        };
    }, [containerRef, handleWheel]);
}
