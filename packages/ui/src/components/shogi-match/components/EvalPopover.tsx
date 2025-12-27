/**
 * 評価値Popoverコンポーネント
 *
 * 評価値をクリックすると開き、読み筋（PV）を表示する
 */

import type { PositionState } from "@shogi/app-core";
import type { ReactElement } from "react";
import { useMemo, useState } from "react";
import { Popover, PopoverContent, PopoverTrigger } from "../../popover";
import type { KifMove } from "../utils/kifFormat";
import { convertPvToDisplay, getEvalTooltipInfo } from "../utils/kifFormat";

interface EvalPopoverProps {
    /** 指し手情報 */
    move: KifMove;
    /** PV変換用の局面（この局面からPVを適用する） */
    position: PositionState;
    /** 評価値表示要素（トリガー） */
    children: ReactElement;
    /** 分岐として追加するコールバック（Phase 2で実装） */
    onAddBranch?: (ply: number, pv: string[]) => void;
    /** 盤面で確認するコールバック（Phase 3で実装） */
    onPreview?: (ply: number, pv: string[]) => void;
}

export function EvalPopover({
    move,
    position,
    children,
    onAddBranch,
    onPreview,
}: EvalPopoverProps): ReactElement {
    const [open, setOpen] = useState(false);

    // PVをKIF形式に変換
    const pvDisplay = useMemo(() => {
        if (!move.pv || move.pv.length === 0) {
            return null;
        }
        return convertPvToDisplay(move.pv, position);
    }, [move.pv, position]);

    // 評価値の詳細情報
    const evalInfo = useMemo(() => {
        return getEvalTooltipInfo(move.evalCp, move.evalMate, move.ply, move.depth);
    }, [move.evalCp, move.evalMate, move.ply, move.depth]);

    // PVがない場合はトリガーのみ表示（Popoverは開かない）
    if (!pvDisplay || pvDisplay.length === 0) {
        return children;
    }

    return (
        <Popover open={open} onOpenChange={setOpen}>
            <PopoverTrigger asChild>{children}</PopoverTrigger>
            <PopoverContent
                className="w-80 p-3"
                side="left"
                align="start"
                onOpenAutoFocus={(e) => e.preventDefault()}
            >
                {/* ヘッダー: 評価値情報 */}
                <div className="flex items-center justify-between mb-3 pb-2 border-b border-border">
                    <div
                        className={`font-medium ${
                            evalInfo.advantage === "sente"
                                ? "text-wafuu-shu"
                                : evalInfo.advantage === "gote"
                                  ? "text-[hsl(210_70%_45%)]"
                                  : ""
                        }`}
                    >
                        {evalInfo.description}
                    </div>
                    <div className="text-muted-foreground text-[11px] space-x-2">
                        {evalInfo.detail && <span>{evalInfo.detail}</span>}
                        {evalInfo.depthText && <span>{evalInfo.depthText}</span>}
                    </div>
                </div>

                {/* 読み筋 */}
                <div className="space-y-2">
                    <div className="text-[11px] font-medium text-muted-foreground">読み筋:</div>
                    <div className="flex flex-wrap gap-1 text-[12px] font-mono">
                        {pvDisplay.map((m, index) => (
                            <span
                                key={`${index}-${m.usiMove}`}
                                className={
                                    m.turn === "sente"
                                        ? "text-wafuu-shu"
                                        : "text-[hsl(210_70%_45%)]"
                                }
                            >
                                {m.displayText}
                                {index < pvDisplay.length - 1 && (
                                    <span className="text-muted-foreground mx-0.5">→</span>
                                )}
                            </span>
                        ))}
                    </div>
                </div>

                {/* アクションボタン（Phase 2, 3で有効化） */}
                {(onPreview || onAddBranch) && (
                    <div className="flex gap-2 mt-3 pt-2 border-t border-border">
                        {onPreview && move.pv && (
                            <button
                                type="button"
                                onClick={() => {
                                    onPreview(move.ply, move.pv ?? []);
                                    setOpen(false);
                                }}
                                className="
                                    flex-1 px-3 py-1.5 text-[11px]
                                    bg-muted hover:bg-muted/80
                                    rounded border border-border
                                    transition-colors cursor-pointer
                                "
                            >
                                <span className="mr-1">&#9654;</span>
                                盤面で確認
                            </button>
                        )}
                        {onAddBranch && move.pv && (
                            <button
                                type="button"
                                onClick={() => {
                                    onAddBranch(move.ply, move.pv ?? []);
                                    setOpen(false);
                                }}
                                className="
                                    flex-1 px-3 py-1.5 text-[11px]
                                    bg-muted hover:bg-muted/80
                                    rounded border border-border
                                    transition-colors cursor-pointer
                                "
                            >
                                <span className="mr-1">&#128194;</span>
                                分岐として保存
                            </button>
                        )}
                    </div>
                )}
            </PopoverContent>
        </Popover>
    );
}
