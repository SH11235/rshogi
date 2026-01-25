/**
 * スマホ向けボトムシートコンポーネント
 *
 * 設定パネルなどを下からスライドして表示する
 * Radix UI Dialog を使用してz-index管理を統一
 */

import { cn } from "@shogi/design-system";
import type { ReactElement, ReactNode } from "react";
import { Button } from "../../button";
import { Dialog, DialogClose, DialogContent, DialogHeader, DialogTitle } from "../../dialog";

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
    /** オーバーレイ表示 */
    overlay?: "dim" | "transparent";
    /** シートの見た目 */
    surface?: "solid" | "glass";
}

const heightStyles = {
    half: { height: "50vh" },
    full: { height: "85vh" },
    // auto の場合も height を設定し、内部の flex レイアウトを機能させる
    // コンテンツが少ない場合は min-content で縮み、多い場合は 85vh でスクロール
    auto: { height: "auto", maxHeight: "85vh" },
} as const;
// Glass surface tuning for mobile overlays/sheets.
// Lower opacity + lower blur makes the board more visible behind the surface.
export const GLASS_SURFACE_OPACITY = 0.05;
export const GLASS_SURFACE_BLUR_PX = 1;

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
    overlay = "dim",
    surface = "solid",
}: BottomSheetProps): ReactElement {
    const overlayStyle =
        overlay === "transparent"
            ? { backgroundColor: "transparent", backdropFilter: "none" }
            : undefined;
    const surfaceStyle =
        surface === "glass"
            ? {
                  backgroundColor: `hsl(var(--background, 0 0% 100%) / ${GLASS_SURFACE_OPACITY})`,
                  backdropFilter: `blur(${GLASS_SURFACE_BLUR_PX}px)`,
              }
            : undefined;
    const surfaceClassName = surface === "glass" ? "" : "bg-background";
    return (
        <Dialog open={open} onOpenChange={onOpenChange}>
            <DialogContent
                overlayStyle={overlayStyle}
                onOpenAutoFocus={(event) => event.preventDefault()}
                data-state={open ? "open" : "closed"}
                className={cn(
                    "fixed bottom-0 left-0 right-0 z-50",
                    "w-screen",
                    "rounded-t-2xl",
                    "overflow-hidden",
                    "duration-200 data-[state=open]:animate-in data-[state=closed]:animate-out",
                    "data-[state=closed]:fade-out data-[state=open]:fade-in",
                    "data-[state=closed]:slide-out-to-bottom data-[state=open]:slide-in-from-bottom",
                )}
                style={{
                    ...heightStyles[height],
                    top: "auto",
                    left: 0,
                    right: 0,
                    bottom: 0,
                    transform: "none",
                    width: "100%",
                    maxWidth: "100%",
                    padding: 0,
                    border: "none",
                    borderRadius: "16px 16px 0 0",
                    overflow: "hidden",
                    display: "flex",
                    flexDirection: "column",
                    ...surfaceStyle,
                }}
            >
                {/* ドラッグハンドル（装飾） */}
                <div className={cn("flex justify-center py-2", surfaceClassName)}>
                    <div className="w-10 h-1 bg-muted rounded-full" />
                </div>

                {/* ヘッダー */}
                <DialogHeader className={cn("px-4 pb-3 border-b border-border", surfaceClassName)}>
                    <DialogTitle className="font-semibold text-lg">{title}</DialogTitle>
                </DialogHeader>

                {/* コンテンツ */}
                <div
                    className={cn(
                        "px-4 pt-4 max-w-full flex-1 min-h-0 overflow-y-auto overscroll-contain touch-pan-y",
                        surfaceClassName,
                    )}
                    style={{ WebkitOverflowScrolling: "touch" }}
                >
                    {children}
                </div>

                {/* 閉じるボタン */}
                <div
                    className={cn(
                        "px-4 pt-4 pb-[calc(1rem+env(safe-area-inset-bottom))] border-t border-border",
                        surfaceClassName,
                    )}
                >
                    <DialogClose asChild>
                        <Button type="button" variant="secondary" className="w-full">
                            閉じる
                        </Button>
                    </DialogClose>
                </div>
            </DialogContent>
        </Dialog>
    );
}
