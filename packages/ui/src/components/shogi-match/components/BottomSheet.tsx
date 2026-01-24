/**
 * スマホ向けボトムシートコンポーネント
 *
 * 設定パネルなどを下からスライドして表示する
 * Radix UI Dialog を使用してz-index管理を統一
 */

import { cn } from "@shogi/design-system";
import type { ReactElement, ReactNode } from "react";
import { Dialog, DialogClose, DialogOverlay, DialogPortal } from "../../dialog";

interface BottomSheetProps {
    /** シートを開くかどうか */
    open: boolean;
    /** 開閉状態変更時のコールバック */
    onOpenChange: (open: boolean) => void;
    /** タイトル */
    title: string;
    /** コンテンツ */
    children: ReactNode;
    /** 高さ: 'half' | 'full' | 'auto' */
    height?: "half" | "full" | "auto";
}

const heightStyles = {
    half: { height: "50vh" },
    full: { height: "85vh" },
    // auto の場合も height を設定し、内部の flex レイアウトを機能させる
    // コンテンツが少ない場合は min-content で縮み、多い場合は 85vh でスクロール
    auto: { height: "auto", maxHeight: "85vh" },
} as const;

/**
 * スマホ向けボトムシートコンポーネント
 * 設定パネルなどを下からスライドして表示する
 */
export function BottomSheet({
    open,
    onOpenChange,
    title,
    children,
    height = "auto",
}: BottomSheetProps): ReactElement {
    return (
        <Dialog open={open} onOpenChange={onOpenChange}>
            <DialogPortal>
                <DialogOverlay />
                <div
                    data-state={open ? "open" : "closed"}
                    className={cn(
                        "fixed bottom-0 left-0 right-0 z-50",
                        "w-screen",
                        "bg-background rounded-t-2xl",
                        "overflow-x-hidden overflow-y-auto",
                        "overscroll-contain touch-pan-y",
                        "duration-200 data-[state=open]:animate-in data-[state=closed]:animate-out",
                        "data-[state=closed]:fade-out data-[state=open]:fade-in",
                        "data-[state=closed]:slide-out-to-bottom data-[state=open]:slide-in-from-bottom",
                    )}
                    style={{
                        ...heightStyles[height],
                        display: "flex",
                        flexDirection: "column",
                    }}
                >
                    {/* ドラッグハンドル（装飾） */}
                    <div className="flex justify-center py-2 sticky top-0 bg-background z-10">
                        <div className="w-10 h-1 bg-muted rounded-full" />
                    </div>

                    {/* ヘッダー */}
                    <div className="px-4 pb-3 border-b border-border font-semibold text-lg">
                        {title}
                    </div>

                    {/* コンテンツ */}
                    {/* min-h-0 は flexbox 内でスクロールを機能させるために必要 */}
                    <div className="p-4 max-w-full flex-1 min-h-0 overflow-auto">{children}</div>

                    {/* 閉じるボタン */}
                    <div className="sticky bottom-0 p-4 bg-background border-t border-border">
                        <DialogClose asChild>
                            <button
                                type="button"
                                className="w-full py-3 rounded-lg bg-muted hover:bg-muted/80 font-medium transition-colors"
                            >
                                閉じる
                            </button>
                        </DialogClose>
                    </div>
                </div>
            </DialogPortal>
        </Dialog>
    );
}
