/**
 * 設定モーダルコンポーネント
 *
 * PC版で対局設定とインポート機能をモーダル表示する
 */

import { cn } from "@shogi/design-system";
import type { ReactElement, ReactNode } from "react";
import { useEffect, useRef } from "react";

interface SettingsModalProps {
    /** モーダルを開くかどうか */
    isOpen: boolean;
    /** 閉じる時のコールバック */
    onClose: () => void;
    /** コンテンツ */
    children: ReactNode;
}

/**
 * PC向け設定モーダル
 * 画面中央に表示される
 */
export function SettingsModal({
    isOpen,
    onClose,
    children,
}: SettingsModalProps): ReactElement | null {
    const modalRef = useRef<HTMLDivElement>(null);
    const closeButtonRef = useRef<HTMLButtonElement>(null);

    // ESCキーで閉じる
    useEffect(() => {
        if (!isOpen) return;

        const handleKeyDown = (e: KeyboardEvent) => {
            if (e.key === "Escape") {
                onClose();
            }
        };

        document.addEventListener("keydown", handleKeyDown);
        return () => document.removeEventListener("keydown", handleKeyDown);
    }, [isOpen, onClose]);

    // フォーカストラップ
    useEffect(() => {
        if (!isOpen || !modalRef.current) return;

        const modal = modalRef.current;
        const focusableElements = modal.querySelectorAll<HTMLElement>(
            'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
        );
        const firstFocusable = focusableElements[0];
        const lastFocusable = focusableElements[focusableElements.length - 1];

        // モーダルが開いたら閉じるボタンにフォーカス
        closeButtonRef.current?.focus();

        const handleTabKey = (e: KeyboardEvent) => {
            if (e.key !== "Tab") return;

            if (e.shiftKey) {
                if (document.activeElement === firstFocusable) {
                    e.preventDefault();
                    lastFocusable?.focus();
                }
            } else {
                if (document.activeElement === lastFocusable) {
                    e.preventDefault();
                    firstFocusable?.focus();
                }
            }
        };

        document.addEventListener("keydown", handleTabKey);
        return () => document.removeEventListener("keydown", handleTabKey);
    }, [isOpen]);

    // 背景スクロールを無効化
    useEffect(() => {
        if (isOpen) {
            const originalOverflow = document.body.style.overflow;
            document.body.style.overflow = "hidden";
            return () => {
                document.body.style.overflow = originalOverflow;
            };
        }
    }, [isOpen]);

    if (!isOpen) return null;

    return (
        <>
            {/* オーバーレイ */}
            <div
                className="fixed inset-0 bg-black/50 z-[999]"
                onClick={onClose}
                aria-hidden="true"
            />

            {/* モーダル本体 */}
            <div
                ref={modalRef}
                role="dialog"
                aria-modal="true"
                aria-labelledby="settings-modal-title"
                className={cn(
                    "fixed top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 z-[1000]",
                    "bg-background rounded-xl shadow-2xl",
                    "max-h-[85vh] overflow-auto",
                    "animate-in fade-in zoom-in-95 duration-200",
                )}
            >
                {/* ヘッダー */}
                <div className="sticky top-0 bg-background border-b border-border px-6 py-4 flex items-center justify-between">
                    <h2 id="settings-modal-title" className="font-bold text-lg">
                        設定
                    </h2>
                    <button
                        ref={closeButtonRef}
                        type="button"
                        onClick={onClose}
                        className="p-2 rounded-lg hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
                        aria-label="閉じる"
                    >
                        <svg
                            xmlns="http://www.w3.org/2000/svg"
                            width="20"
                            height="20"
                            viewBox="0 0 24 24"
                            fill="none"
                            stroke="currentColor"
                            strokeWidth="2"
                            strokeLinecap="round"
                            strokeLinejoin="round"
                            aria-hidden="true"
                        >
                            <line x1="18" y1="6" x2="6" y2="18" />
                            <line x1="6" y1="6" x2="18" y2="18" />
                        </svg>
                    </button>
                </div>

                {/* コンテンツ */}
                <div className="p-6">{children}</div>
            </div>
        </>
    );
}
