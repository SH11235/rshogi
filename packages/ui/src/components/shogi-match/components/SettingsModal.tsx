/**
 * 設定モーダルコンポーネント
 *
 * PC版で対局設定とインポート機能をモーダル表示する
 * Radix UI Dialog を使用してz-index管理を統一
 */

import type { ReactElement, ReactNode } from "react";
import { Dialog, DialogClose, DialogContent, DialogHeader, DialogTitle } from "../../dialog";

interface SettingsModalProps {
    /** モーダルを開くかどうか */
    open: boolean;
    /** 開閉状態変更時のコールバック */
    onOpenChange: (open: boolean) => void;
    /** コンテンツ */
    children: ReactNode;
}

/**
 * PC向け設定モーダル
 * 画面中央に表示される
 */
export function SettingsModal({ open, onOpenChange, children }: SettingsModalProps): ReactElement {
    return (
        <Dialog open={open} onOpenChange={onOpenChange}>
            <DialogContent
                style={{
                    width: "min(520px, calc(100% - 24px))",
                    maxHeight: "85vh",
                    display: "flex",
                    flexDirection: "column",
                    padding: 0,
                }}
            >
                {/* ヘッダー */}
                <DialogHeader
                    style={{
                        position: "sticky",
                        top: 0,
                        backgroundColor: "hsl(var(--background, 0 0% 100%))",
                        borderBottom: "1px solid hsl(var(--border, 0 0% 86%))",
                        padding: "16px 24px",
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                        justifyContent: "space-between",
                    }}
                >
                    <DialogTitle>設定</DialogTitle>
                    <DialogClose asChild>
                        <button
                            type="button"
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
                    </DialogClose>
                </DialogHeader>

                {/* コンテンツ */}
                <div style={{ padding: "24px", overflow: "auto", flex: 1 }}>{children}</div>
            </DialogContent>
        </Dialog>
    );
}
