import { cn } from "@shogi/design-system";
import type { ReactElement, ReactNode } from "react";
import { useEffect, useRef } from "react";

export interface BottomSheetProps {
    /** シートを開くかどうか */
    isOpen: boolean;
    /** 閉じる時のコールバック */
    onClose: () => void;
    /** タイトル */
    title: string;
    /** コンテンツ */
    children: ReactNode;
    /** 高さ: 'half' | 'full' | 'auto' */
    height?: "half" | "full" | "auto";
}

const heightClasses = {
    half: "h-[50vh]",
    full: "h-[90vh]",
    auto: "max-h-[90vh]",
} as const;

/**
 * スマホ向けボトムシートコンポーネント
 * 設定パネルなどを下からスライドして表示する
 */
export function BottomSheet({
    isOpen,
    onClose,
    title,
    children,
    height = "auto",
}: BottomSheetProps): ReactElement | null {
    const sheetRef = useRef<HTMLDivElement>(null);

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

    // フォーカストラップ（アクセシビリティ対応）
    useEffect(() => {
        if (!isOpen || !sheetRef.current) return;

        const sheet = sheetRef.current;
        const focusableElements = sheet.querySelectorAll<HTMLElement>(
            'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
        );
        const firstFocusable = focusableElements[0];
        const lastFocusable = focusableElements[focusableElements.length - 1];

        // シートが開いたら最初の要素にフォーカス
        firstFocusable?.focus();

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

            {/* シート本体 */}
            <div
                ref={sheetRef}
                role="dialog"
                aria-modal="true"
                aria-labelledby="bottom-sheet-title"
                className={cn(
                    "fixed bottom-0 left-0 right-0 z-[1000]",
                    "bg-background rounded-t-2xl",
                    "overflow-y-auto",
                    "transition-transform duration-300 ease-out",
                    "animate-in slide-in-from-bottom",
                    heightClasses[height],
                )}
            >
                {/* ドラッグハンドル（装飾） */}
                <div className="flex justify-center py-2 sticky top-0 bg-background">
                    <div className="w-10 h-1 bg-muted rounded-full" />
                </div>

                {/* ヘッダー */}
                <div
                    id="bottom-sheet-title"
                    className="px-4 pb-3 border-b border-border font-semibold text-lg"
                >
                    {title}
                </div>

                {/* コンテンツ */}
                <div className="p-4">{children}</div>

                {/* 閉じるボタン */}
                <div className="sticky bottom-0 p-4 bg-background border-t border-border">
                    <button
                        type="button"
                        onClick={onClose}
                        className="w-full py-3 rounded-lg bg-muted hover:bg-muted/80 font-medium transition-colors"
                    >
                        閉じる
                    </button>
                </div>
            </div>
        </>
    );
}
