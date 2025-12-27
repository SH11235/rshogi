/**
 * KIF形式棋譜表示パネル
 *
 * 棋譜をKIF形式（日本語表記）で表示し、評価値も合わせて表示する
 */

import type { PositionState } from "@shogi/app-core";
import type { ReactElement } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Switch } from "../../switch";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "../../tooltip";
import type { KifMove } from "../utils/kifFormat";
import { formatEval, getEvalTooltipInfo } from "../utils/kifFormat";
import { EvalPopover } from "./EvalPopover";
import { KifuNavigationToolbar } from "./KifuNavigationToolbar";

/**
 * 評価値データが存在するかチェック
 */
function hasEvalData(kifMoves: KifMove[]): boolean {
    return kifMoves.some((m) => m.evalCp !== undefined || m.evalMate !== undefined);
}

interface BranchInfo {
    hasBranches: boolean;
    currentIndex: number;
    count: number;
    onSwitch: (index: number) => void;
    onPromoteToMain?: () => void;
}

interface NavigationProps {
    /** 現在の手数 */
    currentPly: number;
    /** 最大手数（メインライン） */
    totalPly: number;
    /** 1手戻る */
    onBack: () => void;
    /** 1手進む */
    onForward: () => void;
    /** 最初へ */
    onToStart: () => void;
    /** 最後へ */
    onToEnd: () => void;
    /** 巻き戻し中か */
    isRewound?: boolean;
    /** 分岐情報 */
    branchInfo?: BranchInfo;
    /** 進む操作が可能か（現在ノードに子がある） */
    canGoForward?: boolean;
}

interface KifuPanelProps {
    /** KIF形式の指し手リスト */
    kifMoves: KifMove[];
    /** 現在の手数（ハイライト用） */
    currentPly: number;
    /** 手数クリック時のコールバック（局面ジャンプ用） */
    onPlySelect?: (ply: number) => void;
    /** 評価値を表示するか */
    showEval?: boolean;
    /** 評価値表示の切り替えコールバック */
    onShowEvalChange?: (show: boolean) => void;
    /** KIF形式でコピーするときのコールバック（KIF文字列を返す） */
    onCopyKif?: () => string;
    /** ナビゲーション機能（提供された場合はツールバーを表示） */
    navigation?: NavigationProps;
    /** ナビゲーション無効化（対局中など） */
    navigationDisabled?: boolean;
    /** 分岐マーカー（ply -> 分岐数） */
    branchMarkers?: Map<number, number>;
    /** 局面履歴（各手が指された後の局面、PV表示用） */
    positionHistory?: PositionState[];
    /** PVを分岐として追加するコールバック */
    onAddPvAsBranch?: (ply: number, pv: string[]) => void;
    /** PVを盤面で確認するコールバック */
    onPreviewPv?: (ply: number, pv: string[]) => void;
    /** 指定手数の局面を解析するコールバック（オンデマンド解析用） */
    onAnalyzePly?: (ply: number) => void;
    /** 解析中かどうか */
    isAnalyzing?: boolean;
    /** 現在解析中の手数 */
    analyzingPly?: number;
    /** 一括解析の状態 */
    batchAnalysis?: {
        isRunning: boolean;
        currentIndex: number;
        totalCount: number;
    };
    /** 一括解析を開始するコールバック */
    onStartBatchAnalysis?: () => void;
    /** 一括解析をキャンセルするコールバック */
    onCancelBatchAnalysis?: () => void;
}

/**
 * 評価値ヒントバナー
 * 評価値がOFFだがデータが存在する場合に表示
 */
function EvalHintBanner({
    onEnable,
    onDismiss,
}: {
    onEnable: () => void;
    onDismiss: () => void;
}): ReactElement {
    return (
        <div
            className="
                relative overflow-hidden
                bg-gradient-to-r from-[hsl(var(--wafuu-washi-warm))] to-[hsl(var(--wafuu-washi))]
                border border-[hsl(var(--wafuu-kin)/0.4)]
                rounded-lg px-3 py-2 mb-2
                animate-[slideDown_0.3s_ease-out,fadeIn_0.3s_ease-out]
            "
            style={{
                boxShadow: "0 2px 8px hsl(var(--wafuu-kin) / 0.15)",
            }}
        >
            {/* 金色のアクセントライン */}
            <div className="absolute top-0 left-0 right-0 h-[2px] bg-gradient-to-r from-transparent via-[hsl(var(--wafuu-kin))] to-transparent opacity-60" />

            <div className="flex items-center justify-between gap-2">
                <button
                    type="button"
                    onClick={onEnable}
                    className="
                        flex items-center gap-2 text-[12px] font-medium
                        text-[hsl(var(--wafuu-sumi))] dark:text-[hsl(var(--foreground))]
                        hover:text-[hsl(var(--wafuu-shu))] transition-colors
                        bg-transparent border-none cursor-pointer p-0
                    "
                >
                    <span
                        className="
                            inline-flex items-center justify-center
                            w-5 h-5 rounded-full
                            bg-[hsl(var(--wafuu-kin)/0.2)]
                            text-[hsl(var(--wafuu-kin))]
                            animate-[pulse_2s_ease-in-out_infinite]
                        "
                    >
                        ✦
                    </span>
                    <span>評価値データがあります。表示しますか？</span>
                </button>

                <button
                    type="button"
                    onClick={onDismiss}
                    className="
                        flex items-center justify-center
                        w-5 h-5 rounded-full
                        text-[hsl(var(--muted-foreground))]
                        hover:text-[hsl(var(--foreground))]
                        hover:bg-[hsl(var(--muted))]
                        bg-transparent border-none cursor-pointer
                        transition-colors
                    "
                    aria-label="閉じる"
                >
                    ✕
                </button>
            </div>
        </div>
    );
}

/**
 * 評価値ツールチップの内容
 */
function EvalTooltipContent({
    evalCp,
    evalMate,
    ply,
    depth,
}: {
    evalCp?: number;
    evalMate?: number;
    ply: number;
    depth?: number;
}): ReactElement {
    const info = getEvalTooltipInfo(evalCp, evalMate, ply, depth);

    return (
        <div className="space-y-1">
            <div
                className={`font-medium ${
                    info.advantage === "sente"
                        ? "text-wafuu-shu"
                        : info.advantage === "gote"
                          ? "text-[hsl(210_70%_45%)]"
                          : ""
                }`}
            >
                {info.description}
            </div>
            <div className="text-muted-foreground text-[10px] space-x-2">
                {info.detail && <span>{info.detail}</span>}
                {info.depthText && <span>{info.depthText}</span>}
            </div>
        </div>
    );
}

/**
 * 評価値のスタイルクラスを決定
 */
function getEvalClassName(evalCp?: number, evalMate?: number): string {
    const baseClass = "text-[11px] text-right min-w-12";
    if (evalMate !== undefined && evalMate !== null) {
        return evalMate > 0
            ? `${baseClass} text-wafuu-shu`
            : `${baseClass} text-[hsl(210_70%_45%)]`;
    }
    if (evalCp !== undefined && evalCp !== null) {
        return evalCp >= 0 ? `${baseClass} text-wafuu-shu` : `${baseClass} text-[hsl(210_70%_45%)]`;
    }
    return `${baseClass} text-muted-foreground`;
}

export function KifuPanel({
    kifMoves,
    currentPly,
    onPlySelect,
    showEval = true,
    onShowEvalChange,
    onCopyKif,
    navigation,
    navigationDisabled = false,
    branchMarkers,
    positionHistory,
    onAddPvAsBranch,
    onPreviewPv,
    onAnalyzePly,
    isAnalyzing,
    analyzingPly,
    batchAnalysis,
    onStartBatchAnalysis,
    onCancelBatchAnalysis,
}: KifuPanelProps): ReactElement {
    const listRef = useRef<HTMLDivElement>(null);
    const currentRowRef = useRef<HTMLElement>(null);
    const [copySuccess, setCopySuccess] = useState(false);
    const [hintDismissed, setHintDismissed] = useState(false);

    // 評価値データの存在チェック
    const evalDataExists = useMemo(() => hasEvalData(kifMoves), [kifMoves]);

    // PVがない手の数
    const movesWithoutPv = useMemo(
        () => kifMoves.filter((m) => !m.pv || m.pv.length === 0).length,
        [kifMoves],
    );

    // ヒントバナーを表示するかどうか
    const showHintBanner = !showEval && evalDataExists && !hintDismissed && onShowEvalChange;

    // 現在の手数が変わったら自動スクロール（現在の手を中央に配置）
    useEffect(() => {
        // currentPlyが範囲外の場合はスクロールしない
        if (currentPly < 1 || currentPly > kifMoves.length) return;

        const container = listRef.current;
        const row = currentRowRef.current;
        if (!container || !row) return;

        // コンテナ内での相対位置を計算
        const rowTop = row.offsetTop - container.offsetTop;
        const rowHeight = row.offsetHeight;
        const containerHeight = container.clientHeight;

        // 現在の手をコンテナの中央に配置するスクロール位置を計算
        const targetScrollTop = rowTop - containerHeight / 2 + rowHeight / 2;

        // スクロール位置を設定（0未満にならないよう制限）
        container.scrollTop = Math.max(0, targetScrollTop);
    }, [currentPly, kifMoves.length]);

    // コピーボタンのハンドラ
    const handleCopy = useCallback(async () => {
        if (!onCopyKif) return;

        const kifString = onCopyKif();
        try {
            await navigator.clipboard.writeText(kifString);
            setCopySuccess(true);
            setTimeout(() => setCopySuccess(false), 2000);
        } catch (error) {
            console.error("Failed to copy to clipboard:", error);
        }
    }, [onCopyKif]);

    return (
        <TooltipProvider delayDuration={300}>
            <div className="bg-card border border-border rounded-xl p-3 shadow-lg w-[var(--panel-width)]">
                <div className="font-bold mb-1.5 flex justify-between items-center gap-2">
                    <div className="flex items-center gap-2">
                        <span>棋譜</span>
                        <span className="text-[13px] text-muted-foreground">
                            {kifMoves.length === 0 ? "開始局面" : `${kifMoves.length}手`}
                        </span>
                    </div>
                    <div className="flex items-center gap-2">
                        {/* 評価値表示トグル（強調版） */}
                        {onShowEvalChange && (
                            <label
                                htmlFor="kifu-eval-toggle"
                                className={`
                                relative flex items-center gap-1.5 cursor-pointer
                                px-2 py-1 rounded-md transition-all duration-200
                                ${
                                    evalDataExists && !showEval
                                        ? "bg-[hsl(var(--wafuu-kin)/0.1)] hover:bg-[hsl(var(--wafuu-kin)/0.2)]"
                                        : "hover:bg-muted/50"
                                }
                            `}
                            >
                                {/* 評価値データ存在インジケーター */}
                                {evalDataExists && !showEval && (
                                    <span
                                        className="
                                        absolute -top-1 -right-1
                                        w-2.5 h-2.5 rounded-full
                                        bg-[hsl(var(--wafuu-kin))]
                                        animate-[pulse_2s_ease-in-out_infinite]
                                        shadow-[0_0_6px_hsl(var(--wafuu-kin)/0.6)]
                                    "
                                        aria-hidden="true"
                                    />
                                )}
                                <span
                                    className={`
                                    text-[12px] font-medium transition-colors
                                    ${
                                        evalDataExists && !showEval
                                            ? "text-[hsl(var(--wafuu-kin))]"
                                            : showEval
                                              ? "text-foreground"
                                              : "text-muted-foreground"
                                    }
                                `}
                                >
                                    評価値
                                </span>
                                <Switch
                                    id="kifu-eval-toggle"
                                    checked={showEval}
                                    onCheckedChange={onShowEvalChange}
                                    aria-label="評価値を表示"
                                />
                                {/* 評価値の凡例インフォアイコン */}
                                <Tooltip>
                                    <TooltipTrigger asChild>
                                        <button
                                            type="button"
                                            className="
                                                inline-flex items-center justify-center
                                                w-4 h-4 rounded-full
                                                text-[10px] text-muted-foreground
                                                border border-muted-foreground/30
                                                hover:bg-muted hover:text-foreground
                                                cursor-help transition-colors
                                                bg-transparent
                                            "
                                            aria-label="評価値の見方"
                                        >
                                            ?
                                        </button>
                                    </TooltipTrigger>
                                    <TooltipContent side="bottom" className="max-w-[220px]">
                                        <div className="space-y-1.5 text-[11px]">
                                            <div className="font-medium">評価値の見方</div>
                                            <div className="space-y-0.5">
                                                <div>
                                                    <span className="text-wafuu-shu">+値</span>
                                                    <span className="text-muted-foreground ml-1">
                                                        ☗先手有利
                                                    </span>
                                                </div>
                                                <div>
                                                    <span className="text-[hsl(210_70%_45%)]">
                                                        -値
                                                    </span>
                                                    <span className="text-muted-foreground ml-1">
                                                        ☖後手有利
                                                    </span>
                                                </div>
                                            </div>
                                            <div className="text-muted-foreground text-[10px] pt-1 border-t border-border">
                                                各評価値にホバーで詳細表示
                                            </div>
                                        </div>
                                    </TooltipContent>
                                </Tooltip>
                            </label>
                        )}
                        {/* 一括解析ボタン */}
                        {onStartBatchAnalysis &&
                            kifMoves.length > 0 &&
                            movesWithoutPv > 0 &&
                            !batchAnalysis?.isRunning && (
                                <button
                                    type="button"
                                    className="px-2 py-1 text-[11px] rounded border cursor-pointer transition-colors duration-150 bg-primary/10 text-primary border-primary/30 hover:bg-primary/20"
                                    onClick={onStartBatchAnalysis}
                                    title={`PVがない${movesWithoutPv}手を解析`}
                                >
                                    一括解析
                                </button>
                            )}
                        {onCopyKif && kifMoves.length > 0 && (
                            <button
                                type="button"
                                className={`px-2 py-1 text-[11px] rounded border cursor-pointer transition-colors duration-150 ${
                                    copySuccess
                                        ? "bg-green-600 text-white border-green-600"
                                        : "bg-background text-foreground border-border"
                                }`}
                                onClick={handleCopy}
                                title="KIF形式でクリップボードにコピー"
                            >
                                {copySuccess ? "コピー完了" : "KIFコピー"}
                            </button>
                        )}
                    </div>
                </div>

                {/* ナビゲーションツールバー */}
                {navigation && (
                    <KifuNavigationToolbar
                        currentPly={navigation.currentPly}
                        totalPly={navigation.totalPly}
                        onBack={navigation.onBack}
                        onForward={navigation.onForward}
                        onToStart={navigation.onToStart}
                        onToEnd={navigation.onToEnd}
                        disabled={navigationDisabled}
                        branchInfo={navigation.branchInfo}
                        isRewound={navigation.isRewound}
                        canGoForward={navigation.canGoForward}
                    />
                )}

                {/* 一括解析進捗バナー */}
                {batchAnalysis?.isRunning && (
                    <div className="bg-primary/10 border border-primary/30 rounded-lg px-3 py-2 mb-2">
                        <div className="flex items-center justify-between gap-2 mb-1.5">
                            <div className="flex items-center gap-2 text-[12px] text-primary font-medium">
                                <span className="animate-pulse">●</span>
                                <span>
                                    一括解析中... {batchAnalysis.currentIndex + 1}/
                                    {batchAnalysis.totalCount}
                                </span>
                            </div>
                            {onCancelBatchAnalysis && (
                                <button
                                    type="button"
                                    onClick={onCancelBatchAnalysis}
                                    className="px-2 py-0.5 text-[11px] rounded border cursor-pointer transition-colors bg-background text-foreground border-border hover:bg-muted"
                                >
                                    キャンセル
                                </button>
                            )}
                        </div>
                        {/* プログレスバー */}
                        <div className="h-1.5 bg-primary/20 rounded-full overflow-hidden">
                            <div
                                className="h-full bg-primary transition-all duration-300 ease-out"
                                style={{
                                    width: `${((batchAnalysis.currentIndex + 1) / batchAnalysis.totalCount) * 100}%`,
                                }}
                            />
                        </div>
                    </div>
                )}

                {/* 評価値ヒントバナー */}
                {showHintBanner && (
                    <EvalHintBanner
                        onEnable={() => onShowEvalChange(true)}
                        onDismiss={() => setHintDismissed(true)}
                    />
                )}

                <div ref={listRef} className="max-h-60 overflow-auto my-2">
                    {kifMoves.length === 0 ? (
                        <div className="text-[13px] text-muted-foreground text-center py-4">
                            まだ指し手がありません
                        </div>
                    ) : (
                        kifMoves.map((move, index) => {
                            const isCurrent = move.ply === currentPly;
                            const isPastCurrent = navigation?.isRewound && move.ply > currentPly;
                            const evalText = showEval
                                ? formatEval(move.evalCp, move.evalMate, move.ply)
                                : "";
                            const hasBranch = branchMarkers?.has(move.ply);
                            const branchCount = branchMarkers?.get(move.ply);
                            // この手に対応する局面（手が指された後の局面）
                            const position = positionHistory?.[index];
                            // PVがあるかどうか
                            const hasPv = move.pv && move.pv.length > 0;
                            // EvalPopoverを使用するか（PVがあるか、解析機能がある場合）
                            const useEvalPopover = position && (hasPv || onAnalyzePly);

                            // 評価値表示コンポーネント
                            const evalSpan = (
                                <span
                                    className={`${getEvalClassName(move.evalCp, move.evalMate)} ${isPastCurrent ? "opacity-50" : ""} ${useEvalPopover ? "cursor-pointer" : "cursor-help"}`}
                                >
                                    {evalText}
                                </span>
                            );

                            const content = (
                                <>
                                    <span
                                        className={`text-right text-xs ${isPastCurrent ? "text-muted-foreground/50" : "text-muted-foreground"}`}
                                    >
                                        {move.ply}
                                        {hasBranch && (
                                            <span
                                                className="ml-0.5 text-wafuu-shu"
                                                title={`${branchCount}つの分岐`}
                                            >
                                                ◆
                                            </span>
                                        )}
                                    </span>
                                    <span
                                        className={`font-medium ${isPastCurrent ? "text-muted-foreground/50" : ""}`}
                                    >
                                        {move.displayText}
                                    </span>
                                    {showEval &&
                                        evalText &&
                                        (useEvalPopover && position ? (
                                            <EvalPopover
                                                move={move}
                                                position={position}
                                                onAddBranch={onAddPvAsBranch}
                                                onPreview={onPreviewPv}
                                                onAnalyze={onAnalyzePly}
                                                isAnalyzing={isAnalyzing}
                                                analyzingPly={analyzingPly}
                                            >
                                                {evalSpan}
                                            </EvalPopover>
                                        ) : (
                                            <Tooltip>
                                                <TooltipTrigger asChild>
                                                    {/* 親要素（行クリック）へのイベント伝播を防ぐ */}
                                                    <button
                                                        type="button"
                                                        className="inline bg-transparent border-none p-0 m-0 font-inherit text-inherit"
                                                        onClick={(e) => e.stopPropagation()}
                                                        onKeyDown={(e) => e.stopPropagation()}
                                                    >
                                                        {evalSpan}
                                                    </button>
                                                </TooltipTrigger>
                                                <TooltipContent
                                                    side="left"
                                                    className="max-w-[200px]"
                                                >
                                                    <EvalTooltipContent
                                                        evalCp={move.evalCp}
                                                        evalMate={move.evalMate}
                                                        ply={move.ply}
                                                        depth={move.depth}
                                                    />
                                                </TooltipContent>
                                            </Tooltip>
                                        ))}
                                </>
                            );

                            const rowClassName = `grid grid-cols-[32px_1fr_auto] gap-1 items-center px-1 py-0.5 text-[13px] font-mono rounded ${
                                isCurrent ? "bg-accent" : ""
                            }`;

                            if (onPlySelect) {
                                return (
                                    <button
                                        type="button"
                                        key={move.ply}
                                        ref={
                                            isCurrent
                                                ? (currentRowRef as React.RefObject<HTMLButtonElement>)
                                                : undefined
                                        }
                                        className={`${rowClassName} w-full text-left bg-transparent border-none cursor-pointer hover:bg-accent/50`}
                                        onClick={() => onPlySelect(move.ply)}
                                    >
                                        {content}
                                    </button>
                                );
                            }

                            return (
                                <div
                                    key={move.ply}
                                    ref={
                                        isCurrent
                                            ? (currentRowRef as React.RefObject<HTMLDivElement>)
                                            : undefined
                                    }
                                    className={rowClassName}
                                >
                                    {content}
                                </div>
                            );
                        })
                    )}
                </div>
            </div>
        </TooltipProvider>
    );
}
