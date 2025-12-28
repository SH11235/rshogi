/**
 * 評価値Popoverコンポーネント
 *
 * 評価値をクリックすると開き、読み筋（PV）を表示する
 * PVがない場合は解析ボタンを表示する
 */

import type { KifuTree, PositionState } from "@shogi/app-core";
import type { ReactElement } from "react";
import { useMemo, useState } from "react";
import { Popover, PopoverContent, PopoverTrigger } from "../../popover";
import { Tooltip, TooltipContent, TooltipTrigger } from "../../tooltip";
import { comparePvWithMainLine, type PvMainLineComparison } from "../utils/branchTreeUtils";
import type { KifMove } from "../utils/kifFormat";
import { convertPvToDisplay, getEvalTooltipInfo } from "../utils/kifFormat";

interface EvalPopoverProps {
    /** 指し手情報 */
    move: KifMove;
    /** PV変換用の局面（この局面からPVを適用する） */
    position: PositionState;
    /** 評価値表示要素（トリガー） */
    children: ReactElement;
    /** 分岐として追加するコールバック（ply: 分岐を追加する手数, pv: 追加するPV部分） */
    onAddBranch?: (ply: number, pv: string[]) => void;
    /** 盤面で確認するコールバック */
    onPreview?: (ply: number, pv: string[], evalCp?: number, evalMate?: number) => void;
    /** 指定手数の局面を解析するコールバック */
    onAnalyze?: (ply: number) => void;
    /** 解析中かどうか */
    isAnalyzing?: boolean;
    /** 現在解析中の手数 */
    analyzingPly?: number;
    /** 棋譜ツリー（PVと本譜の比較用） */
    kifuTree?: KifuTree;
}

export function EvalPopover({
    move,
    position,
    children,
    onAddBranch,
    onPreview,
    onAnalyze,
    isAnalyzing,
    analyzingPly,
    kifuTree,
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

    // PVと本譜の比較結果
    const pvComparison = useMemo((): PvMainLineComparison | null => {
        if (!kifuTree || !move.pv || move.pv.length === 0) {
            return null;
        }
        return comparePvWithMainLine(kifuTree, move.ply, move.pv);
    }, [kifuTree, move.ply, move.pv]);

    // この手数が解析中かどうか
    const isThisPlyAnalyzing = isAnalyzing && analyzingPly === move.ply;

    // PVがなく、解析機能もない場合はトリガーのみ表示
    const hasPv = pvDisplay && pvDisplay.length > 0;
    if (!hasPv && !onAnalyze) {
        return children;
    }

    return (
        <Popover open={open} onOpenChange={setOpen}>
            <Tooltip>
                <TooltipTrigger asChild>
                    <PopoverTrigger asChild>
                        <button
                            type="button"
                            className="inline bg-transparent border-none p-0 m-0 font-inherit text-inherit cursor-pointer"
                            onClick={(e) => e.stopPropagation()}
                            onKeyDown={(e) => e.stopPropagation()}
                            onPointerDown={(e) => e.stopPropagation()}
                        >
                            {children}
                        </button>
                    </PopoverTrigger>
                </TooltipTrigger>
                {/* Popoverが開いていない時のみTooltipを表示 */}
                {!open && (
                    <TooltipContent
                        side="left"
                        className="max-w-[200px] cursor-pointer"
                        onClick={() => setOpen(true)}
                    >
                        <div className="space-y-1">
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
                            <div className="text-muted-foreground text-[10px] space-x-2">
                                {evalInfo.detail && <span>{evalInfo.detail}</span>}
                                {evalInfo.depthText && <span>{evalInfo.depthText}</span>}
                            </div>
                            <div className="text-muted-foreground text-[10px] pt-1 border-t border-border">
                                クリックで詳細表示
                            </div>
                        </div>
                    </TooltipContent>
                )}
            </Tooltip>
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

                {/* 読み筋がある場合 */}
                {hasPv && pvDisplay && (
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
                )}

                {/* 読み筋がない場合は解析ボタンを表示 */}
                {!hasPv && onAnalyze && (
                    <div className="space-y-2">
                        <div className="text-[11px] text-muted-foreground">読み筋がありません</div>
                        <button
                            type="button"
                            onClick={() => {
                                onAnalyze(move.ply);
                            }}
                            disabled={isThisPlyAnalyzing}
                            className="
                                w-full px-3 py-2 text-[12px]
                                bg-primary text-primary-foreground
                                hover:bg-primary/90
                                disabled:opacity-50 disabled:cursor-not-allowed
                                rounded border border-border
                                transition-colors cursor-pointer
                            "
                        >
                            {isThisPlyAnalyzing ? (
                                <span>解析中...</span>
                            ) : (
                                <>
                                    <span className="mr-1">&#128269;</span>
                                    この局面を解析する
                                </>
                            )}
                        </button>
                    </div>
                )}

                {/* アクションボタン（PVがある場合のみ） */}
                {hasPv && (onPreview || onAddBranch) && (
                    <div className="flex gap-2 mt-3 pt-2 border-t border-border">
                        {onPreview && move.pv && (
                            <button
                                type="button"
                                onClick={() => {
                                    onPreview(move.ply, move.pv ?? [], move.evalCp, move.evalMate);
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
                            <>
                                {/* 本譜と完全一致の場合 */}
                                {pvComparison?.type === "identical" && (
                                    <div
                                        className="
                                            flex-1 px-3 py-1.5 text-[11px] text-center
                                            bg-muted/50 text-muted-foreground
                                            rounded border border-border
                                        "
                                    >
                                        <span className="mr-1">✓</span>
                                        本譜通り
                                    </div>
                                )}
                                {/* 途中から分岐する場合 */}
                                {pvComparison?.type === "diverges_later" &&
                                    pvComparison.divergePly !== undefined &&
                                    pvComparison.divergeIndex !== undefined && (
                                        <button
                                            type="button"
                                            onClick={() => {
                                                // 分岐点から先のPVのみを追加
                                                const pvFromDiverge = move.pv?.slice(
                                                    pvComparison.divergeIndex,
                                                );
                                                if (
                                                    pvFromDiverge &&
                                                    pvFromDiverge.length > 0 &&
                                                    pvComparison.divergePly !== undefined
                                                ) {
                                                    onAddBranch(
                                                        pvComparison.divergePly,
                                                        pvFromDiverge,
                                                    );
                                                }
                                                setOpen(false);
                                            }}
                                            className="
                                                flex-1 px-3 py-1.5 text-[11px]
                                                bg-[hsl(var(--wafuu-kin)/0.1)] hover:bg-[hsl(var(--wafuu-kin)/0.2)]
                                                text-[hsl(var(--wafuu-sumi))]
                                                rounded border border-[hsl(var(--wafuu-kin)/0.3)]
                                                transition-colors cursor-pointer
                                            "
                                        >
                                            <span className="mr-1">&#128194;</span>
                                            {pvComparison.divergePly + 1}手目から分岐を追加
                                        </button>
                                    )}
                                {/* 最初から異なる場合（従来通り） */}
                                {(pvComparison?.type === "diverges_first" || !pvComparison) && (
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
                            </>
                        )}
                    </div>
                )}
            </PopoverContent>
        </Popover>
    );
}
