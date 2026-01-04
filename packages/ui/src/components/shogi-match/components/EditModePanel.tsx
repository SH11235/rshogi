import type { ReactElement } from "react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "../../collapsible";

interface EditModePanelProps {
    // パネル表示状態
    isOpen: boolean;
    onOpenChange: (open: boolean) => void;

    // メッセージ
    message: string | null;
}

export function EditModePanel({ isOpen, onOpenChange, message }: EditModePanelProps): ReactElement {
    return (
        <Collapsible open={isOpen} onOpenChange={onOpenChange}>
            <div className="w-[var(--panel-width)] overflow-hidden rounded-xl border-2 border-[hsl(var(--wafuu-border))] bg-[hsl(var(--wafuu-washi-warm))] shadow-lg">
                <CollapsibleTrigger asChild>
                    <button
                        type="button"
                        aria-label="操作ヘルプを開閉"
                        className={`flex w-full cursor-pointer items-center justify-between gap-3 border-none bg-gradient-to-br from-[hsl(var(--wafuu-washi))] to-[hsl(var(--wafuu-washi-warm))] px-4 py-3.5 transition-all duration-200 ${
                            isOpen ? "border-b border-[hsl(var(--wafuu-border))]" : ""
                        }`}
                    >
                        <span className="text-lg font-bold tracking-wide text-[hsl(var(--wafuu-sumi))]">
                            操作ヘルプ
                        </span>
                        <span
                            className={`shrink-0 text-xl text-[hsl(var(--wafuu-kincha))] transition-transform duration-200 ${
                                isOpen ? "rotate-180" : "rotate-0"
                            }`}
                        >
                            ▼
                        </span>
                    </button>
                </CollapsibleTrigger>
                <CollapsibleContent>
                    <div className="flex flex-col gap-3.5 p-4">
                        {message && (
                            <div className="rounded-lg border-l-[3px] border-l-[hsl(var(--wafuu-shu))] bg-[hsl(var(--wafuu-washi))] p-2.5 text-[13px] text-[hsl(var(--wafuu-shu))]">
                                {message}
                            </div>
                        )}
                        <div className="rounded-lg border-l-[3px] border-l-[hsl(var(--wafuu-kin))] bg-[hsl(var(--wafuu-washi))] p-3 text-xs text-[hsl(var(--wafuu-sumi-light))]">
                            <div className="mb-1.5 font-semibold text-[hsl(var(--wafuu-sumi))]">
                                編集モード操作
                            </div>
                            <ul className="m-0 list-disc pl-5 leading-relaxed">
                                <li>
                                    <strong>駒を配置:</strong> 持ち駒から盤面にドラッグ
                                </li>
                                <li>
                                    <strong>駒を移動:</strong> 盤面の駒をドラッグして移動
                                </li>
                                <li>
                                    <strong>駒を削除:</strong> 盤外にドラッグ
                                </li>
                                <li>
                                    <strong>成/不成切替:</strong> 盤上の駒を右クリック or
                                    ダブルクリック
                                </li>
                                <li>
                                    <strong>成り付与:</strong> Shift を押しながらドラッグ
                                </li>
                                <li>
                                    <strong>持ち駒調整:</strong> ±ボタンで駒数を変更
                                </li>
                            </ul>
                        </div>
                    </div>
                </CollapsibleContent>
            </div>
        </Collapsible>
    );
}
