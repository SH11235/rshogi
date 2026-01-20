/**
 * KIF形式棋譜表示パネル
 *
 * 棋譜をKIF形式（日本語表記）で表示し、評価値も合わせて表示する
 */

import type { KifuTree, PositionState } from "@shogi/app-core";
import { detectParallelism } from "@shogi/app-core";
import type { ReactElement } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Popover, PopoverContent, PopoverTrigger } from "../../popover";
import { Switch } from "../../switch";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "../../tooltip";
import type { AnalysisSettings, AutoAnalyzeMode } from "../types";
import type { BranchSummary, FlatTreeNode, PvMainLineComparison } from "../utils/branchTreeUtils";
import {
    comparePvWithMainLine,
    findExistingBranchForPv,
    getAllBranches,
    getBranchesByPly,
    getBranchMoves,
} from "../utils/branchTreeUtils";
import type { KifMove, PvDisplayMove, PvEvalInfo } from "../utils/kifFormat";
import { convertPvToDisplay, formatEval, getEvalTooltipInfo } from "../utils/kifFormat";
import { KifuNavigationToolbar } from "./KifuNavigationToolbar";

/** 表示モード */
type ViewMode = "main" | "branches" | "selectedBranch";

/** ネストされた分岐のインデント幅（px） */
const NEST_INDENT_PX = 16;

/** 選択中の分岐情報 */
interface SelectedBranch {
    /** 分岐のノードID */
    nodeId: string;
    /** タブ表示用のラベル */
    tabLabel: string;
}

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
    onPreviewPv?: (ply: number, pv: string[], evalCp?: number, evalMate?: number) => void;
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
        inProgress?: number[]; // 並列解析中の手番号
    };
    /** 一括解析を開始するコールバック（本譜のみ） */
    onStartBatchAnalysis?: () => void;
    /** ツリー全体の一括解析を開始するコールバック */
    onStartTreeBatchAnalysis?: (options?: { mainLineOnly?: boolean }) => void;
    /** 一括解析をキャンセルするコールバック */
    onCancelBatchAnalysis?: () => void;
    /** 解析設定 */
    analysisSettings?: AnalysisSettings;
    /** 解析設定変更コールバック */
    onAnalysisSettingsChange?: (settings: AnalysisSettings) => void;
    /** 棋譜ツリー（ツリービュー用） */
    kifuTree?: KifuTree;
    /** ノードクリック時のコールバック（ツリービュー用） */
    onNodeClick?: (nodeId: string) => void;
    /** 分岐切り替え時のコールバック（ツリービュー用） */
    onBranchSwitch?: (parentNodeId: string, branchIndex: number) => void;
    /** 分岐内のノードを解析するコールバック */
    onAnalyzeNode?: (nodeId: string) => void;
    /** 分岐全体を一括解析するコールバック */
    onAnalyzeBranch?: (branchNodeId: string) => void;
    /** 追加のクラス名（高さ調整用） */
    className?: string;
    /** 最後に追加された分岐の情報（この分岐に直接遷移する） */
    lastAddedBranchInfo?: { ply: number; firstMove: string } | null;
    /** lastAddedBranchInfoを処理したことを通知するコールバック */
    onLastAddedBranchHandled?: () => void;
    /** 選択中の分岐が変更されたときのコールバック（キーボードナビゲーション用） */
    onSelectedBranchChange?: (branchNodeId: string | null) => void;
    /** 現在位置がメインライン上にあるか（PV分岐追加の制御用） */
    isOnMainLine?: boolean;
    /** 手の詳細を選択したときのコールバック（右パネル表示用） */
    onMoveDetailSelect?: (move: KifMove | null, position: PositionState | null) => void;
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
 * インライン分岐リスト（本譜ビューで分岐を展開表示）
 */
function InlineBranchList({
    branches,
    onBranchClick,
    onAnalyzeBranch,
}: {
    branches: BranchSummary[];
    onBranchClick: (branch: BranchSummary) => void;
    onAnalyzeBranch?: (branchNodeId: string) => void;
}): ReactElement {
    return (
        <div className="ml-6 pl-2 border-l-2 border-[hsl(var(--wafuu-shu)/0.3)] my-0.5">
            {branches.map((branch, index) => {
                const isLast = index === branches.length - 1;
                return (
                    <div key={branch.nodeId} className="flex items-center gap-1 py-0.5">
                        {/* ツリー罫線 */}
                        <span className="text-[11px] text-muted-foreground/60 font-mono">
                            {isLast ? "└─" : "├─"}
                        </span>
                        {/* 分岐ボタン */}
                        <button
                            type="button"
                            onClick={(e) => {
                                e.stopPropagation();
                                onBranchClick(branch);
                            }}
                            className="
                                flex items-center gap-1.5
                                text-[12px] text-left
                                px-1.5 py-0.5 rounded
                                hover:bg-[hsl(var(--wafuu-washi))]
                                transition-colors cursor-pointer
                                bg-transparent border-none
                            "
                        >
                            <span className="font-medium text-[hsl(var(--wafuu-shu))]">
                                {branch.displayText}
                            </span>
                            <span className="text-[10px] text-muted-foreground">
                                ({branch.branchLength}手)
                            </span>
                        </button>
                        {/* 分岐解析ボタン */}
                        {onAnalyzeBranch && (
                            <button
                                type="button"
                                onClick={(e) => {
                                    e.stopPropagation();
                                    onAnalyzeBranch(branch.nodeId);
                                }}
                                className="text-[10px] px-1 py-0.5 rounded bg-muted hover:bg-accent text-muted-foreground hover:text-foreground transition-colors"
                                title="この分岐を一括解析"
                            >
                                解析
                            </button>
                        )}
                    </div>
                );
            })}
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
 * 展開された指し手の詳細表示コンポーネント
 * EvalPopoverで表示していた内容をインラインで表示する
 */
function ExpandedMoveDetails({
    move,
    position,
    onAddBranch,
    onPreview,
    onAnalyze,
    isAnalyzing,
    analyzingPly,
    kifuTree,
    onCollapse,
    isOnMainLine = true,
}: {
    move: KifMove;
    position: PositionState;
    onAddBranch?: (ply: number, pv: string[]) => void;
    onPreview?: (ply: number, pv: string[], evalCp?: number, evalMate?: number) => void;
    onAnalyze?: (ply: number) => void;
    isAnalyzing?: boolean;
    analyzingPly?: number;
    kifuTree?: KifuTree;
    onCollapse: () => void;
    /** 現在位置がメインライン上にあるか（falseの場合はPV分岐追加を無効化） */
    isOnMainLine?: boolean;
}): ReactElement {
    // 複数PVがある場合はリストで表示、なければ従来の単一PVを使用
    const pvList = useMemo((): PvEvalInfo[] => {
        // multiPvEvalsがある場合はそれを使用
        if (move.multiPvEvals && move.multiPvEvals.length > 0) {
            return move.multiPvEvals;
        }
        // 従来の単一PVからフォールバック
        if (move.pv && move.pv.length > 0) {
            return [
                {
                    multipv: 1,
                    evalCp: move.evalCp,
                    evalMate: move.evalMate,
                    depth: move.depth,
                    pv: move.pv,
                },
            ];
        }
        return [];
    }, [move.multiPvEvals, move.pv, move.evalCp, move.evalMate, move.depth]);

    // 評価値の詳細情報（ヘッダー用、最良の候補=multipv1のもの）
    const evalInfo = useMemo(() => {
        const bestPv = pvList[0];
        return getEvalTooltipInfo(
            bestPv?.evalCp ?? move.evalCp,
            bestPv?.evalMate ?? move.evalMate,
            move.ply,
            bestPv?.depth ?? move.depth,
        );
    }, [pvList, move.evalCp, move.evalMate, move.ply, move.depth]);

    // この手数が解析中かどうか
    const isThisPlyAnalyzing = isAnalyzing && analyzingPly === move.ply;

    const hasPv = pvList.length > 0;
    const hasMultiplePv = pvList.length > 1;

    return (
        <section
            className="
                mt-1 mx-1 p-3 rounded-lg
                bg-[hsl(var(--wafuu-washi))] dark:bg-[hsl(var(--card))]
                border border-[hsl(var(--border))]
                shadow-sm
                animate-[slideDown_0.15s_ease-out]
            "
            aria-label={`${move.ply}手目の詳細`}
        >
            {/* ヘッダー: 評価値情報と閉じるボタン */}
            <div className="flex items-center justify-between mb-2 pb-2 border-b border-border">
                <div className="flex items-center gap-2">
                    <span className="text-[11px] text-muted-foreground">{move.ply}手目</span>
                    <span
                        className={`font-medium text-[13px] ${
                            evalInfo.advantage === "sente"
                                ? "text-wafuu-shu"
                                : evalInfo.advantage === "gote"
                                  ? "text-[hsl(210_70%_45%)]"
                                  : ""
                        }`}
                    >
                        {evalInfo.description}
                    </span>
                    {hasMultiplePv && (
                        <span className="text-[10px] text-muted-foreground bg-muted px-1.5 py-0.5 rounded">
                            {pvList.length}候補
                        </span>
                    )}
                </div>
                <div className="flex items-center gap-2">
                    <div className="text-muted-foreground text-[10px] space-x-1.5">
                        {evalInfo.detail && <span>{evalInfo.detail}</span>}
                        {evalInfo.depthText && <span>{evalInfo.depthText}</span>}
                    </div>
                    <button
                        type="button"
                        onClick={onCollapse}
                        className="
                            p-1 rounded hover:bg-muted
                            text-muted-foreground hover:text-foreground
                            transition-colors cursor-pointer
                            bg-transparent border-none
                        "
                        aria-label="閉じる"
                    >
                        <svg
                            xmlns="http://www.w3.org/2000/svg"
                            width="14"
                            height="14"
                            viewBox="0 0 24 24"
                            fill="none"
                            stroke="currentColor"
                            strokeWidth="2"
                            strokeLinecap="round"
                            strokeLinejoin="round"
                            aria-hidden="true"
                        >
                            <polyline points="18 15 12 9 6 15" />
                        </svg>
                    </button>
                </div>
            </div>

            {/* 複数PV候補リスト */}
            {hasPv && (
                <div className="space-y-2">
                    {pvList.map((pv) => (
                        <PvCandidateItem
                            key={pv.multipv}
                            pv={pv}
                            position={position}
                            ply={move.ply}
                            onAddBranch={onAddBranch}
                            onPreview={onPreview}
                            isOnMainLine={isOnMainLine}
                            kifuTree={kifuTree}
                        />
                    ))}
                </div>
            )}

            {/* 読み筋がない場合は解析ボタンを表示 */}
            {!hasPv && onAnalyze && (
                <div className="space-y-2">
                    <div className="text-[11px] text-muted-foreground">読み筋がありません</div>
                    <button
                        type="button"
                        onClick={() => onAnalyze(move.ply)}
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
                                <span className="mr-1">🔍</span>
                                この局面を解析する
                            </>
                        )}
                    </button>
                </div>
            )}
        </section>
    );
}

/**
 * 単一のPV候補を表示するコンポーネント
 */
function PvCandidateItem({
    pv,
    position,
    ply,
    onAddBranch,
    onPreview,
    isOnMainLine,
    kifuTree,
}: {
    pv: PvEvalInfo;
    position: PositionState;
    ply: number;
    onAddBranch?: (ply: number, pvMoves: string[]) => void;
    onPreview?: (ply: number, pvMoves: string[], evalCp?: number, evalMate?: number) => void;
    isOnMainLine: boolean;
    kifuTree?: KifuTree;
}): ReactElement {
    // PVをKIF形式に変換
    const pvDisplay = useMemo((): PvDisplayMove[] | null => {
        if (!pv.pv || pv.pv.length === 0) {
            return null;
        }
        return convertPvToDisplay(pv.pv, position);
    }, [pv.pv, position]);

    // 評価値の詳細情報
    const evalInfo = useMemo(() => {
        return getEvalTooltipInfo(pv.evalCp, pv.evalMate, ply, pv.depth);
    }, [pv.evalCp, pv.evalMate, ply, pv.depth]);

    // PVと本譜の比較結果
    const pvComparison = useMemo((): PvMainLineComparison | null => {
        if (!kifuTree || !pv.pv || pv.pv.length === 0) {
            return null;
        }
        return comparePvWithMainLine(kifuTree, ply, pv.pv);
    }, [kifuTree, ply, pv.pv]);

    // 分岐追加時のPVが既存分岐と一致するかをチェック
    const existingBranchNodeId = useMemo((): string | null => {
        if (!kifuTree || !pv.pv || pv.pv.length === 0 || !pvComparison) {
            return null;
        }

        if (pvComparison.type === "diverges_later" && pvComparison.divergePly !== undefined) {
            const pvFromDiverge = pv.pv.slice(pvComparison.divergeIndex);
            return findExistingBranchForPv(kifuTree, pvComparison.divergePly, pvFromDiverge);
        }

        if (pvComparison.type === "diverges_first") {
            return findExistingBranchForPv(kifuTree, ply, pv.pv);
        }

        return null;
    }, [kifuTree, ply, pv.pv, pvComparison]);

    const hasPv = pvDisplay && pvDisplay.length > 0;

    return (
        <div
            className="
                border border-border rounded-lg p-2
                bg-[hsl(var(--wafuu-washi)/0.3)] dark:bg-[hsl(var(--muted)/0.3)]
            "
        >
            {/* ヘッダー: 候補番号 + 評価値 */}
            <div className="flex items-center gap-2 mb-1">
                <span className="text-[11px] font-medium bg-muted px-1.5 py-0.5 rounded">
                    候補{pv.multipv}
                </span>
                <span
                    className={`font-medium text-[13px] ${
                        evalInfo.advantage === "sente"
                            ? "text-wafuu-shu"
                            : evalInfo.advantage === "gote"
                              ? "text-[hsl(210_70%_45%)]"
                              : ""
                    }`}
                >
                    {formatEval(pv.evalCp, pv.evalMate, ply)}
                </span>
                {pv.depth && (
                    <span className="text-[10px] text-muted-foreground">深さ{pv.depth}</span>
                )}
            </div>

            {/* 読み筋 */}
            {hasPv && (
                <div className="flex flex-wrap gap-1 text-[12px] font-mono mb-2">
                    {pvDisplay.map((m, index) => (
                        <span
                            key={`${index}-${m.usiMove}`}
                            className={
                                m.turn === "sente" ? "text-wafuu-shu" : "text-[hsl(210_70%_45%)]"
                            }
                        >
                            {m.displayText}
                            {index < pvDisplay.length - 1 && (
                                <span className="text-muted-foreground mx-0.5">→</span>
                            )}
                        </span>
                    ))}
                </div>
            )}

            {/* アクションボタン */}
            {/* hasPv が true なら pv.pv は必ず存在する（pvDisplay の生成条件より） */}
            {hasPv && (onPreview || onAddBranch) && (
                <div className="flex gap-2">
                    {onPreview && (
                        <button
                            type="button"
                            onClick={() => onPreview(ply, pv.pv ?? [], pv.evalCp, pv.evalMate)}
                            className="
                                flex-1 px-2 py-1 text-[11px]
                                bg-muted hover:bg-muted/80
                                rounded border border-border
                                transition-colors cursor-pointer
                            "
                        >
                            <span className="mr-1">▶</span>
                            盤面で確認
                        </button>
                    )}
                    {onAddBranch &&
                        (isOnMainLine ? (
                            <>
                                {/* 本譜と完全一致の場合 */}
                                {pvComparison?.type === "identical" && (
                                    <div
                                        className="
                                            flex-1 px-2 py-1 text-[11px] text-center
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
                                    pvComparison.divergeIndex !== undefined &&
                                    (existingBranchNodeId ? (
                                        <div
                                            className="
                                                flex-1 px-2 py-1 text-[11px] text-center
                                                bg-muted/50 text-muted-foreground
                                                rounded border border-border
                                            "
                                        >
                                            <span className="mr-1">✓</span>
                                            分岐追加済み
                                        </div>
                                    ) : (
                                        <button
                                            type="button"
                                            onClick={() => {
                                                const pvFromDiverge = pv.pv?.slice(
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
                                            }}
                                            className="
                                                flex-1 px-2 py-1 text-[11px]
                                                bg-[hsl(var(--wafuu-kin)/0.1)] hover:bg-[hsl(var(--wafuu-kin)/0.2)]
                                                text-[hsl(var(--wafuu-sumi))]
                                                rounded border border-[hsl(var(--wafuu-kin)/0.3)]
                                                transition-colors cursor-pointer
                                            "
                                        >
                                            <span className="mr-1">📂</span>
                                            {pvComparison.divergePly + 1}手目から分岐
                                        </button>
                                    ))}
                                {/* 最初から異なる場合 */}
                                {(pvComparison?.type === "diverges_first" || !pvComparison) &&
                                    (existingBranchNodeId ? (
                                        <div
                                            className="
                                                flex-1 px-2 py-1 text-[11px] text-center
                                                bg-muted/50 text-muted-foreground
                                                rounded border border-border
                                            "
                                        >
                                            <span className="mr-1">✓</span>
                                            分岐追加済み
                                        </div>
                                    ) : (
                                        <button
                                            type="button"
                                            onClick={() => onAddBranch(ply, pv.pv ?? [])}
                                            className="
                                                flex-1 px-2 py-1 text-[11px]
                                                bg-muted hover:bg-muted/80
                                                rounded border border-border
                                                transition-colors cursor-pointer
                                            "
                                        >
                                            <span className="mr-1">📂</span>
                                            分岐として保存
                                        </button>
                                    ))}
                            </>
                        ) : (
                            <div
                                className="
                                    flex-1 px-2 py-1 text-[11px] text-center
                                    bg-muted/30 text-muted-foreground
                                    rounded border border-border/50
                                "
                                title="分岐上にいるため、本譜への分岐追加は利用できません"
                            >
                                <span className="mr-1 opacity-50">📂</span>
                                本譜に戻ると分岐追加可能
                            </div>
                        ))}
                </div>
            )}
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

/**
 * 並列ワーカー数の選択肢
 */
const PARALLEL_WORKER_OPTIONS: { value: number; label: string }[] = [
    { value: 0, label: "自動" },
    { value: 1, label: "1" },
    { value: 2, label: "2" },
    { value: 3, label: "3" },
    { value: 4, label: "4" },
];

/**
 * 解析時間の選択肢
 */
const ANALYSIS_TIME_OPTIONS: { value: number; label: string }[] = [
    { value: 500, label: "0.5秒" },
    { value: 1000, label: "1秒" },
    { value: 2000, label: "2秒" },
    { value: 3000, label: "3秒" },
];

/** 解析対象の選択肢 */
type AnalysisTarget = "mainOnly" | "includeBranches";

/**
 * 一括解析ドロップダウン
 */
function BatchAnalysisDropdown({
    movesWithoutPv,
    analysisSettings,
    onAnalysisSettingsChange,
    onStartBatchAnalysis,
    onStartTreeBatchAnalysis,
    hasBranches,
}: {
    movesWithoutPv: number;
    analysisSettings: AnalysisSettings;
    onAnalysisSettingsChange: (settings: AnalysisSettings) => void;
    onStartBatchAnalysis: () => void;
    onStartTreeBatchAnalysis?: (options?: { mainLineOnly?: boolean }) => void;
    hasBranches: boolean;
}): ReactElement {
    const [isOpen, setIsOpen] = useState(false);
    const [analysisTarget, setAnalysisTarget] = useState<AnalysisTarget>("mainOnly");
    const parallelismConfig = detectParallelism();

    const handleParallelWorkersChange = (value: number) => {
        onAnalysisSettingsChange({
            ...analysisSettings,
            parallelWorkers: value,
        });
    };

    const handleAnalysisTimeChange = (value: number) => {
        onAnalysisSettingsChange({
            ...analysisSettings,
            batchAnalysisTimeMs: value,
        });
    };

    const handleStart = () => {
        setIsOpen(false);
        if (analysisTarget === "includeBranches" && onStartTreeBatchAnalysis) {
            onStartTreeBatchAnalysis({ mainLineOnly: false });
        } else {
            onStartBatchAnalysis();
        }
    };

    const isDisabled = movesWithoutPv === 0;

    return (
        <Popover open={isOpen} onOpenChange={isDisabled ? undefined : setIsOpen}>
            <PopoverTrigger asChild>
                <button
                    type="button"
                    disabled={isDisabled}
                    className={`relative w-7 h-7 flex items-center justify-center text-[14px] rounded border transition-colors duration-150 ${
                        isDisabled
                            ? "cursor-not-allowed opacity-40 bg-muted text-muted-foreground border-border"
                            : "cursor-pointer bg-primary/10 text-primary border-primary/30 hover:bg-primary/20"
                    }`}
                    aria-label={
                        isDisabled ? "解析対象の手がありません" : `一括解析: ${movesWithoutPv}手`
                    }
                >
                    ⚡{/* 自動解析有効時のインジケーター */}
                    {!isDisabled && analysisSettings.autoAnalyzeMode !== "off" && (
                        <span className="absolute -top-1 -right-1 w-2 h-2 bg-[hsl(var(--wafuu-kin))] rounded-full" />
                    )}
                </button>
            </PopoverTrigger>
            <PopoverContent side="bottom" align="end" className="w-64 p-3">
                <div className="space-y-3">
                    <div className="font-medium text-sm">一括解析</div>
                    <div className="text-xs text-muted-foreground">
                        読み筋がない{movesWithoutPv}手を解析します
                    </div>

                    {/* 分岐作成時の自動解析オプション */}
                    <div
                        className={`p-2 rounded-lg border transition-colors ${
                            analysisSettings.autoAnalyzeMode !== "off"
                                ? "bg-[hsl(var(--wafuu-kin)/0.1)] border-[hsl(var(--wafuu-kin)/0.3)]"
                                : "bg-muted/30 border-border"
                        }`}
                    >
                        <div className="space-y-1.5">
                            <div className="text-xs font-medium text-foreground">
                                分岐作成時の自動解析
                            </div>
                            <select
                                value={analysisSettings.autoAnalyzeMode}
                                onChange={(e) =>
                                    onAnalysisSettingsChange({
                                        ...analysisSettings,
                                        autoAnalyzeMode: e.target.value as AutoAnalyzeMode,
                                    })
                                }
                                className="w-full px-2 py-1 text-xs rounded border border-border bg-background"
                            >
                                <option value="off">オフ</option>
                                <option value="delayed">操作が落ち着いてから</option>
                                <option value="immediate">すぐに解析</option>
                            </select>
                            <div className="text-[10px] text-muted-foreground">
                                {analysisSettings.autoAnalyzeMode === "off" &&
                                    "手動で解析ボタンを押したときのみ"}
                                {analysisSettings.autoAnalyzeMode === "delayed" &&
                                    "分岐入力後、数秒待ってから解析開始"}
                                {analysisSettings.autoAnalyzeMode === "immediate" &&
                                    "分岐作成と同時に解析開始（電池消費大）"}
                            </div>
                        </div>
                    </div>

                    {/* 解析対象の選択（分岐がある場合のみ表示） */}
                    {hasBranches && onStartTreeBatchAnalysis && (
                        <div className="space-y-1.5">
                            <div className="text-xs font-medium text-foreground">解析対象</div>
                            <div className="flex gap-1 flex-wrap">
                                <button
                                    type="button"
                                    onClick={() => setAnalysisTarget("mainOnly")}
                                    className={`px-2 py-1 rounded text-xs transition-colors ${
                                        analysisTarget === "mainOnly"
                                            ? "bg-primary text-primary-foreground"
                                            : "bg-muted text-muted-foreground hover:bg-muted/80"
                                    }`}
                                >
                                    本譜のみ
                                </button>
                                <button
                                    type="button"
                                    onClick={() => setAnalysisTarget("includeBranches")}
                                    className={`px-2 py-1 rounded text-xs transition-colors ${
                                        analysisTarget === "includeBranches"
                                            ? "bg-primary text-primary-foreground"
                                            : "bg-muted text-muted-foreground hover:bg-muted/80"
                                    }`}
                                >
                                    分岐を含む
                                </button>
                            </div>
                        </div>
                    )}

                    {/* 並列数設定 */}
                    <div className="space-y-1.5">
                        <div className="text-xs font-medium text-foreground">並列数</div>
                        <div className="flex gap-1 flex-wrap">
                            {PARALLEL_WORKER_OPTIONS.map((opt) => (
                                <button
                                    key={opt.value}
                                    type="button"
                                    onClick={() => handleParallelWorkersChange(opt.value)}
                                    className={`px-2 py-1 rounded text-xs transition-colors ${
                                        analysisSettings.parallelWorkers === opt.value
                                            ? "bg-primary text-primary-foreground"
                                            : "bg-muted text-muted-foreground hover:bg-muted/80"
                                    }`}
                                >
                                    {opt.value === 0
                                        ? `自動(${parallelismConfig.recommendedWorkers})`
                                        : opt.label}
                                </button>
                            ))}
                        </div>
                    </div>

                    {/* 解析時間設定 */}
                    <div className="space-y-1.5">
                        <div className="text-xs font-medium text-foreground">1手あたり解析時間</div>
                        <div className="flex gap-1 flex-wrap">
                            {ANALYSIS_TIME_OPTIONS.map((opt) => (
                                <button
                                    key={opt.value}
                                    type="button"
                                    onClick={() => handleAnalysisTimeChange(opt.value)}
                                    className={`px-2 py-1 rounded text-xs transition-colors ${
                                        analysisSettings.batchAnalysisTimeMs === opt.value
                                            ? "bg-primary text-primary-foreground"
                                            : "bg-muted text-muted-foreground hover:bg-muted/80"
                                    }`}
                                >
                                    {opt.label}
                                </button>
                            ))}
                        </div>
                    </div>

                    {/* 候補手数（MultiPV）設定 */}
                    <div className="space-y-1.5">
                        <div className="text-xs font-medium text-foreground">候補手数</div>
                        <div className="flex gap-1 flex-wrap">
                            {[1, 2, 3, 4, 5].map((n) => (
                                <button
                                    key={n}
                                    type="button"
                                    onClick={() =>
                                        onAnalysisSettingsChange({
                                            ...analysisSettings,
                                            multiPv: n,
                                        })
                                    }
                                    className={`px-2 py-1 rounded text-xs transition-colors ${
                                        analysisSettings.multiPv === n
                                            ? "bg-primary text-primary-foreground"
                                            : "bg-muted text-muted-foreground hover:bg-muted/80"
                                    }`}
                                >
                                    {n}
                                </button>
                            ))}
                        </div>
                        <div className="text-[10px] text-muted-foreground">
                            各手で表示する候補の数
                        </div>
                    </div>

                    <div className="text-[10px] text-muted-foreground">
                        検出コア数: {parallelismConfig.detectedConcurrency}
                    </div>

                    {/* 開始ボタン */}
                    <button
                        type="button"
                        onClick={handleStart}
                        className="w-full py-2 rounded bg-primary text-primary-foreground text-sm font-medium hover:bg-primary/90 transition-colors"
                    >
                        解析開始
                    </button>
                </div>
            </PopoverContent>
        </Popover>
    );
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
    onStartTreeBatchAnalysis,
    onCancelBatchAnalysis,
    analysisSettings,
    onAnalysisSettingsChange,
    kifuTree,
    onNodeClick,
    onBranchSwitch: _onBranchSwitch,
    onAnalyzeNode,
    onAnalyzeBranch,
    lastAddedBranchInfo,
    onLastAddedBranchHandled,
    onSelectedBranchChange,
    isOnMainLine = true,
    onMoveDetailSelect,
}: KifuPanelProps): ReactElement {
    // _onBranchSwitch: 将来的に分岐切り替え機能で使用予定
    const listRef = useRef<HTMLDivElement>(null);
    const currentRowRef = useRef<HTMLElement>(null);
    const [copySuccess, setCopySuccess] = useState(false);
    const [hintDismissed, setHintDismissed] = useState(false);
    const [viewMode, setViewMode] = useState<ViewMode>("main");
    // 選択中の分岐情報
    const [selectedBranch, setSelectedBranch] = useState<SelectedBranch | null>(null);
    // 本譜ビューのスクロール位置を保存
    const mainScrollPositionRef = useRef<number>(0);

    // 分岐一覧を取得
    const branches = useMemo<BranchSummary[]>(() => {
        if (!kifuTree) return [];
        return getAllBranches(kifuTree);
    }, [kifuTree]);

    // 分岐があるか
    const hasBranches = branches.length > 0;

    // 手数ごとの分岐をグルーピング（インライン表示用）
    const branchesByPlyMap = useMemo(() => {
        if (!kifuTree) return new Map<number, BranchSummary[]>();
        return getBranchesByPly(kifuTree);
    }, [kifuTree]);

    // 展開されている手数のセット（折りたたみ状態管理）
    const [expandedPlies, setExpandedPlies] = useState<Set<number>>(new Set());

    // 詳細展開中の手数（null = 非展開）
    const [expandedMoveDetail, setExpandedMoveDetail] = useState<number | null>(null);

    // 折りたたみトグル関数
    const togglePlyExpansion = useCallback((ply: number) => {
        setExpandedPlies((prev) => {
            const next = new Set(prev);
            if (next.has(ply)) {
                next.delete(ply);
            } else {
                next.add(ply);
            }
            return next;
        });
    }, []);

    // 詳細展開トグル関数
    const toggleMoveDetailExpansion = useCallback((ply: number) => {
        setExpandedMoveDetail((prev) => (prev === ply ? null : ply));
    }, []);

    // 選択中の分岐の手順を取得
    const selectedBranchMoves = useMemo<FlatTreeNode[]>(() => {
        if (!kifuTree || !selectedBranch) return [];
        return getBranchMoves(kifuTree, selectedBranch.nodeId);
    }, [kifuTree, selectedBranch]);

    // 処理済みの分岐情報を追跡するref（重複処理防止）
    const processedBranchInfoRef = useRef<{ ply: number; firstMove: string } | null>(null);

    // 分岐が追加されたら直接「選択分岐」ビューに遷移
    useEffect(() => {
        if (!lastAddedBranchInfo) {
            processedBranchInfoRef.current = null;
            return;
        }

        // 既に同じ情報を処理済みの場合はスキップ（重複実行防止）
        if (
            processedBranchInfoRef.current &&
            processedBranchInfoRef.current.ply === lastAddedBranchInfo.ply &&
            processedBranchInfoRef.current.firstMove === lastAddedBranchInfo.firstMove
        ) {
            return;
        }

        // ply + firstMove で分岐を検索
        const branchInList = branches.find((b) => {
            if (b.ply !== lastAddedBranchInfo.ply) return false;
            const node = kifuTree?.nodes.get(b.nodeId);
            return node?.usiMove === lastAddedBranchInfo.firstMove;
        });

        if (branchInList) {
            // 処理済みとしてマーク
            processedBranchInfoRef.current = lastAddedBranchInfo;
            // 追加された分岐を選択して「選択分岐」ビューに遷移
            // （スクロール位置はviewMode遷移検知のuseEffectで自動保存される）
            setSelectedBranch({
                nodeId: branchInList.nodeId,
                tabLabel: branchInList.tabLabel,
            });
            setViewMode("selectedBranch");
            // 処理完了を通知
            onLastAddedBranchHandled?.();
        }
    }, [lastAddedBranchInfo, branches, kifuTree, onLastAddedBranchHandled]);

    // 分岐がなくなった場合は本譜ビューに戻す＆分岐状態をクリア
    useEffect(() => {
        if (branches.length === 0 && viewMode !== "main") {
            setViewMode("main");
            setSelectedBranch(null);
        }
    }, [branches.length, viewMode]);

    // 選択中の分岐が変更されたら親に通知（キーボードナビゲーション用）
    useEffect(() => {
        // selectedBranchビューで分岐が選択されている場合のみnodeIdを通知
        // それ以外の場合はnullを通知（本譜に沿って進む）
        const branchNodeId =
            viewMode === "selectedBranch" && selectedBranch ? selectedBranch.nodeId : null;
        onSelectedBranchChange?.(branchNodeId);
    }, [viewMode, selectedBranch, onSelectedBranchChange]);

    // 前回のビューモードを追跡するref
    const prevViewModeRef = useRef<ViewMode>(viewMode);

    // main → 非main への遷移時にスクロール位置を保存
    useEffect(() => {
        if (prevViewModeRef.current === "main" && viewMode !== "main") {
            // 本譜から別ビューへ: スクロール位置を保存
            if (listRef.current) {
                mainScrollPositionRef.current = listRef.current.scrollTop;
            }
        }
        prevViewModeRef.current = viewMode;
    }, [viewMode]);

    // ビューモード切り替えハンドラ
    const handleViewModeChange = useCallback((newMode: ViewMode) => {
        setViewMode(newMode);
    }, []);

    // 分岐を選択するハンドラ
    const handleSelectBranch = useCallback((branch: BranchSummary) => {
        setSelectedBranch({
            nodeId: branch.nodeId,
            tabLabel: branch.tabLabel,
        });
        setViewMode("selectedBranch");
    }, []);

    // インライン分岐クリック時のハンドラ（ノードに移動して分岐ビューに切り替え）
    const handleInlineBranchClick = useCallback(
        (branch: BranchSummary) => {
            // ノードに移動
            onNodeClick?.(branch.nodeId);
            // 選択した分岐として設定し、分岐ビューに切り替え
            setSelectedBranch({
                nodeId: branch.nodeId,
                tabLabel: branch.tabLabel,
            });
            setViewMode("selectedBranch");
        },
        [onNodeClick],
    );

    // 本譜ビューに戻ったときにスクロール位置を復元
    useEffect(() => {
        if (viewMode === "main" && listRef.current && mainScrollPositionRef.current > 0) {
            // 少し遅延させてDOMが更新された後に復元
            requestAnimationFrame(() => {
                if (listRef.current) {
                    listRef.current.scrollTop = mainScrollPositionRef.current;
                }
            });
        }
    }, [viewMode]);

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
                        {/* 一括解析ボタン（ドロップダウン） - 常に表示してレイアウトシフトを防止 */}
                        {onStartBatchAnalysis &&
                            kifMoves.length > 0 &&
                            !batchAnalysis?.isRunning &&
                            analysisSettings &&
                            onAnalysisSettingsChange && (
                                <BatchAnalysisDropdown
                                    movesWithoutPv={movesWithoutPv}
                                    analysisSettings={analysisSettings}
                                    onAnalysisSettingsChange={onAnalysisSettingsChange}
                                    onStartBatchAnalysis={onStartBatchAnalysis}
                                    onStartTreeBatchAnalysis={onStartTreeBatchAnalysis}
                                    hasBranches={
                                        kifuTree
                                            ? Array.from(kifuTree.nodes.values()).some(
                                                  (n) => n.children.length > 1,
                                              )
                                            : false
                                    }
                                />
                            )}
                        {/* KIFコピーボタン（アイコン） */}
                        {onCopyKif && kifMoves.length > 0 && (
                            <Tooltip>
                                <TooltipTrigger asChild>
                                    <button
                                        type="button"
                                        className={`w-7 h-7 flex items-center justify-center text-[14px] rounded border cursor-pointer transition-colors duration-150 ${
                                            copySuccess
                                                ? "bg-green-600 text-white border-green-600"
                                                : "bg-background text-foreground border-border hover:bg-muted"
                                        }`}
                                        onClick={handleCopy}
                                        aria-label="KIF形式でコピー"
                                    >
                                        {copySuccess ? "✓" : "📋"}
                                    </button>
                                </TooltipTrigger>
                                <TooltipContent side="bottom">
                                    <div className="text-[11px]">
                                        {copySuccess ? "コピー完了!" : "KIF形式でコピー"}
                                    </div>
                                </TooltipContent>
                            </Tooltip>
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

                {/* ビューモード切り替えタブ（分岐がある場合のみ表示） */}
                {hasBranches && (
                    <div className="flex border-b border-border mb-1">
                        {/* 本譜タブ */}
                        <button
                            type="button"
                            onClick={() => handleViewModeChange("main")}
                            className={`
                                flex-1 py-1.5 text-[12px] transition-all duration-150
                                relative
                                ${
                                    viewMode === "main"
                                        ? "text-[hsl(var(--wafuu-shu))] font-medium"
                                        : "text-muted-foreground hover:text-foreground hover:bg-[hsl(var(--wafuu-washi))]"
                                }
                            `}
                        >
                            本譜
                            {viewMode === "main" && (
                                <span className="absolute bottom-[-1px] left-[20%] right-[20%] h-0.5 bg-[hsl(var(--wafuu-shu))] rounded-t" />
                            )}
                        </button>
                        {/* 分岐一覧タブ */}
                        <button
                            type="button"
                            onClick={() => handleViewModeChange("branches")}
                            className={`
                                flex-1 py-1.5 text-[12px] transition-all duration-150
                                relative
                                ${
                                    viewMode === "branches"
                                        ? "text-[hsl(var(--wafuu-shu))] font-medium"
                                        : "text-muted-foreground hover:text-foreground hover:bg-[hsl(var(--wafuu-washi))]"
                                }
                            `}
                        >
                            分岐一覧
                            <span className="ml-1 text-[10px] text-muted-foreground">
                                ({branches.length})
                            </span>
                            {viewMode === "branches" && (
                                <span className="absolute bottom-[-1px] left-[20%] right-[20%] h-0.5 bg-[hsl(var(--wafuu-shu))] rounded-t" />
                            )}
                        </button>
                        {/* 選択した分岐タブ（分岐が選択されている場合のみ表示） */}
                        {selectedBranch && (
                            <button
                                type="button"
                                onClick={() => handleViewModeChange("selectedBranch")}
                                className={`
                                    flex-1 py-1.5 text-[12px] transition-all duration-150
                                    relative max-w-[40%] truncate
                                    ${
                                        viewMode === "selectedBranch"
                                            ? "text-[hsl(var(--wafuu-shu))] font-medium"
                                            : "text-muted-foreground hover:text-foreground hover:bg-[hsl(var(--wafuu-washi))]"
                                    }
                                `}
                                title={selectedBranch.tabLabel}
                            >
                                {selectedBranch.tabLabel}
                                {viewMode === "selectedBranch" && (
                                    <span className="absolute bottom-[-1px] left-[20%] right-[20%] h-0.5 bg-[hsl(var(--wafuu-shu))] rounded-t" />
                                )}
                            </button>
                        )}
                    </div>
                )}

                {/* 一括解析進捗バナー */}
                {batchAnalysis?.isRunning && (
                    <section
                        className="bg-primary/10 border border-primary/30 rounded-lg px-3 py-2 mb-2"
                        aria-label={`一括解析中: ${batchAnalysis.currentIndex}/${batchAnalysis.totalCount}手完了`}
                    >
                        <div className="flex items-center justify-between gap-2 mb-1.5">
                            <div className="flex items-center gap-2 text-[12px] text-primary font-medium">
                                <span className="animate-pulse">●</span>
                                <span>
                                    一括解析中... {batchAnalysis.currentIndex}/
                                    {batchAnalysis.totalCount}
                                    {batchAnalysis.inProgress &&
                                        batchAnalysis.inProgress.length > 1 &&
                                        ` (${batchAnalysis.inProgress.length}手並列)`}
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
                        <div
                            className="h-1.5 bg-primary/20 rounded-full overflow-hidden"
                            role="progressbar"
                            aria-valuemin={0}
                            aria-valuemax={batchAnalysis.totalCount}
                            aria-valuenow={batchAnalysis.currentIndex}
                        >
                            <div
                                className="h-full bg-primary transition-all duration-300 ease-out"
                                style={{
                                    width: `${(batchAnalysis.currentIndex / batchAnalysis.totalCount) * 100}%`,
                                }}
                            />
                        </div>
                    </section>
                )}

                {/* 評価値ヒントバナー */}
                {showHintBanner && (
                    <EvalHintBanner
                        onEnable={() => onShowEvalChange(true)}
                        onDismiss={() => setHintDismissed(true)}
                    />
                )}

                {/* 分岐一覧ビュー */}
                {viewMode === "branches" && (
                    <div className="max-h-[var(--kifu-panel-max-h,70vh)] overflow-auto my-2">
                        {branches.length === 0 ? (
                            <div className="text-[13px] text-muted-foreground text-center py-4">
                                分岐がありません
                            </div>
                        ) : (
                            <div className="space-y-1">
                                {branches.map((branch) => (
                                    <button
                                        key={branch.nodeId}
                                        type="button"
                                        onClick={() => handleSelectBranch(branch)}
                                        className={`
                                            w-full text-left px-3 py-2 rounded-lg
                                            border border-border
                                            transition-all duration-150
                                            hover:bg-[hsl(var(--wafuu-washi))] hover:border-[hsl(var(--wafuu-shu)/0.3)]
                                            ${selectedBranch?.nodeId === branch.nodeId ? "bg-[hsl(var(--wafuu-kin)/0.1)] border-[hsl(var(--wafuu-kin)/0.3)]" : "bg-card"}
                                        `}
                                        style={{
                                            marginLeft: branch.nestLevel * NEST_INDENT_PX,
                                        }}
                                    >
                                        <div className="flex items-center justify-between gap-2">
                                            <div className="flex flex-col gap-0.5">
                                                {branch.parentPath && (
                                                    <span className="text-[10px] text-muted-foreground">
                                                        {branch.parentPath}
                                                    </span>
                                                )}
                                                <div className="flex items-center gap-2">
                                                    <span className="text-[11px] text-muted-foreground min-w-[2.5rem]">
                                                        {branch.ply + 1}手目
                                                    </span>
                                                    <span className="text-[13px] font-medium">
                                                        {branch.displayText}
                                                    </span>
                                                </div>
                                            </div>
                                            <span className="text-[10px] text-muted-foreground">
                                                {branch.branchLength}手
                                            </span>
                                        </div>
                                    </button>
                                ))}
                            </div>
                        )}
                    </div>
                )}

                {/* 選択した分岐ビュー */}
                {viewMode === "selectedBranch" && selectedBranch && (
                    <div className="my-2">
                        {/* 分岐ヘッダー: 解析ボタン（分岐部分が1手以上ある場合のみ表示） */}
                        {(() => {
                            const branchMoveCount = selectedBranchMoves.filter(
                                (n) => n.nestLevel > 0,
                            ).length;
                            if (!onAnalyzeBranch || branchMoveCount === 0) return null;
                            return (
                                <div className="flex items-center justify-between gap-2 px-2 py-1.5 mb-1 bg-[hsl(var(--wafuu-washi))] rounded-lg border border-[hsl(var(--border))]">
                                    <span className="text-[11px] text-muted-foreground">
                                        {branchMoveCount}手の分岐
                                    </span>
                                    <button
                                        type="button"
                                        onClick={() => onAnalyzeBranch(selectedBranch.nodeId)}
                                        className="
                                        px-3 py-1 text-[11px] font-medium
                                        bg-primary text-primary-foreground
                                        hover:bg-primary/90
                                        rounded transition-colors cursor-pointer
                                    "
                                    >
                                        この分岐を解析
                                    </button>
                                </div>
                            );
                        })()}
                        <div className="max-h-[var(--kifu-panel-branch-max-h,calc(70vh-40px))] overflow-auto">
                            {selectedBranchMoves.length === 0 ? (
                                <div className="text-[13px] text-muted-foreground text-center py-4">
                                    分岐データがありません
                                </div>
                            ) : (
                                selectedBranchMoves.map((node, index) => {
                                    const isCurrent = node.isCurrent;
                                    const evalText = showEval
                                        ? formatEval(node.evalCp, node.evalMate, node.ply)
                                        : "";
                                    const isBranchPart = node.nestLevel > 0;
                                    // 分岐開始点かどうか（前の手が本譜で現在が分岐）
                                    const isBranchStart =
                                        isBranchPart &&
                                        index > 0 &&
                                        selectedBranchMoves[index - 1].nestLevel === 0;

                                    return (
                                        <div key={node.nodeId}>
                                            {/* 分岐開始の区切り線 */}
                                            {isBranchStart && (
                                                <div className="flex items-center gap-2 my-1.5 px-1">
                                                    <div className="flex-1 h-px bg-[hsl(var(--wafuu-shu)/0.3)]" />
                                                    <span className="text-[10px] text-[hsl(var(--wafuu-shu))]">
                                                        {node.ply}手目から分岐
                                                    </span>
                                                    {onAnalyzeBranch && (
                                                        <button
                                                            type="button"
                                                            className="text-[10px] px-1.5 py-0.5 rounded bg-[hsl(var(--wafuu-shu)/0.15)] hover:bg-[hsl(var(--wafuu-shu)/0.3)] text-[hsl(var(--wafuu-shu))] transition-colors"
                                                            onClick={(e) => {
                                                                e.stopPropagation();
                                                                onAnalyzeBranch(node.nodeId);
                                                            }}
                                                            title="この分岐の全手を一括解析"
                                                        >
                                                            分岐を解析
                                                        </button>
                                                    )}
                                                    <div className="flex-1 h-px bg-[hsl(var(--wafuu-shu)/0.3)]" />
                                                </div>
                                            )}
                                            <div
                                                role="option"
                                                className={`
                                                grid grid-cols-[32px_1fr_auto_auto] gap-1 items-center px-1 py-0.5 text-[13px] font-mono rounded
                                                cursor-pointer hover:bg-accent/50
                                                ${isCurrent ? "bg-accent" : ""}
                                                ${isBranchPart ? "border-l-2 border-[hsl(var(--wafuu-shu)/0.5)] ml-1" : ""}
                                            `}
                                                onClick={() => onNodeClick?.(node.nodeId)}
                                                onKeyDown={(e) => {
                                                    if (e.key === "Enter" || e.key === " ") {
                                                        e.preventDefault();
                                                        onNodeClick?.(node.nodeId);
                                                    }
                                                }}
                                                tabIndex={0}
                                            >
                                                <span className="text-right text-xs text-muted-foreground">
                                                    {node.ply}
                                                </span>
                                                <span className="font-medium">
                                                    {node.displayText}
                                                </span>
                                                {showEval && evalText && (
                                                    <span
                                                        className={getEvalClassName(
                                                            node.evalCp,
                                                            node.evalMate,
                                                        )}
                                                    >
                                                        {evalText}
                                                    </span>
                                                )}
                                                {/* 解析ボタン（評価値がない場合に表示） */}
                                                {onAnalyzeNode &&
                                                    !evalText &&
                                                    analyzingPly !== node.ply && (
                                                        <button
                                                            type="button"
                                                            className="text-[10px] px-1.5 py-0.5 rounded bg-muted hover:bg-accent text-muted-foreground hover:text-foreground transition-colors"
                                                            onClick={(e) => {
                                                                e.stopPropagation();
                                                                onAnalyzeNode(node.nodeId);
                                                            }}
                                                            title="この手を解析"
                                                        >
                                                            解析
                                                        </button>
                                                    )}
                                                {analyzingPly === node.ply && (
                                                    <span className="text-[10px] text-muted-foreground animate-pulse">
                                                        解析中...
                                                    </span>
                                                )}
                                            </div>
                                        </div>
                                    );
                                })
                            )}
                        </div>
                    </div>
                )}

                {/* 本譜ビュー（メインライン） */}
                {viewMode === "main" && (
                    <div
                        ref={listRef}
                        className="max-h-[var(--kifu-panel-max-h,70vh)] overflow-auto my-2"
                    >
                        {kifMoves.length === 0 ? (
                            <div className="text-[13px] text-muted-foreground text-center py-4">
                                まだ指し手がありません
                            </div>
                        ) : (
                            kifMoves.map((move, index) => {
                                const isCurrent = move.ply === currentPly;
                                const isPastCurrent =
                                    navigation?.isRewound && move.ply > currentPly;
                                const evalText = showEval
                                    ? formatEval(move.evalCp, move.evalMate, move.ply)
                                    : "";
                                const hasBranch = branchMarkers?.has(move.ply);
                                const branchCount = branchMarkers?.get(move.ply);
                                // この手での分岐一覧
                                const branchesAtPly = branchesByPlyMap.get(move.ply) ?? [];
                                const isExpanded = expandedPlies.has(move.ply);
                                // この手に対応する局面（手が指された後の局面）
                                const position = positionHistory?.[index];
                                // PVがあるかどうか
                                const hasPv = move.pv && move.pv.length > 0;
                                // 詳細を展開するか（PVがあるか、解析機能がある場合）
                                const canExpand = position && (hasPv || onAnalyzePly);
                                // この行が詳細展開中か
                                const isDetailExpanded = expandedMoveDetail === move.ply;

                                // 評価値表示コンポーネント
                                const evalSpan = (
                                    <Tooltip>
                                        <TooltipTrigger asChild>
                                            <span
                                                className={`${getEvalClassName(move.evalCp, move.evalMate)} ${isPastCurrent ? "opacity-50" : ""}`}
                                            >
                                                {evalText}
                                            </span>
                                        </TooltipTrigger>
                                        {!isDetailExpanded && (
                                            <TooltipContent side="left" className="max-w-[200px]">
                                                <EvalTooltipContent
                                                    evalCp={move.evalCp}
                                                    evalMate={move.evalMate}
                                                    ply={move.ply}
                                                    depth={move.depth}
                                                />
                                            </TooltipContent>
                                        )}
                                    </Tooltip>
                                );

                                // 行クリックハンドラ（ply選択 + 詳細展開/外部通知）
                                const handleRowClick = () => {
                                    // まず局面を選択
                                    onPlySelect?.(move.ply);

                                    // 外部コールバックがある場合は詳細を右パネルに表示
                                    if (onMoveDetailSelect && canExpand && position) {
                                        // 同じ手をもう一度クリックしたら選択解除
                                        if (isDetailExpanded) {
                                            onMoveDetailSelect(null, null);
                                            setExpandedMoveDetail(null);
                                        } else {
                                            onMoveDetailSelect(move, position);
                                            setExpandedMoveDetail(move.ply);
                                        }
                                        return;
                                    }

                                    // 外部コールバックがない場合は従来のインライン展開
                                    if (canExpand) {
                                        toggleMoveDetailExpansion(move.ply);
                                    }
                                };

                                const content = (
                                    <>
                                        <span
                                            className={`text-right text-xs ${isPastCurrent ? "text-muted-foreground/50" : "text-muted-foreground"}`}
                                        >
                                            {move.ply}
                                            {hasBranch && (
                                                <button
                                                    type="button"
                                                    className="ml-0.5 text-wafuu-shu cursor-pointer hover:opacity-70 bg-transparent border-none p-0"
                                                    onClick={(e) => {
                                                        e.stopPropagation();
                                                        togglePlyExpansion(move.ply);
                                                    }}
                                                    title={`${branchCount}つの分岐を${isExpanded ? "閉じる" : "開く"}`}
                                                >
                                                    {isExpanded ? "▼" : "◆"}
                                                </button>
                                            )}
                                        </span>
                                        <span
                                            className={`font-medium ${isPastCurrent ? "text-muted-foreground/50" : ""}`}
                                        >
                                            {move.displayText}
                                        </span>
                                        {showEval && evalText && evalSpan}
                                        {/* 展開可能インジケータ */}
                                        {canExpand && (
                                            <span
                                                className={`text-[10px] transition-transform duration-150 ${isDetailExpanded ? "rotate-180" : ""} ${isPastCurrent ? "text-muted-foreground/30" : "text-muted-foreground/60"}`}
                                            >
                                                ▾
                                            </span>
                                        )}
                                    </>
                                );

                                const rowClassName = `grid grid-cols-[32px_1fr_auto_auto] gap-1 items-center px-1 py-0.5 text-[13px] font-mono rounded ${
                                    isCurrent ? "bg-accent" : ""
                                } ${isDetailExpanded ? "bg-accent/70" : ""}`;

                                // インライン分岐リスト（展開時のみ表示）
                                const inlineBranches =
                                    hasBranch && isExpanded && branchesAtPly.length > 0 ? (
                                        <InlineBranchList
                                            branches={branchesAtPly}
                                            onBranchClick={handleInlineBranchClick}
                                            onAnalyzeBranch={onAnalyzeBranch}
                                        />
                                    ) : null;

                                // 詳細展開コンテンツ（外部コールバックがある場合は右パネルに表示するためインライン非表示）
                                const expandedDetails =
                                    !onMoveDetailSelect && isDetailExpanded && position ? (
                                        <div>
                                            <ExpandedMoveDetails
                                                move={move}
                                                position={position}
                                                onAddBranch={onAddPvAsBranch}
                                                onPreview={onPreviewPv}
                                                onAnalyze={onAnalyzePly}
                                                isAnalyzing={isAnalyzing}
                                                analyzingPly={analyzingPly}
                                                kifuTree={kifuTree}
                                                onCollapse={() => setExpandedMoveDetail(null)}
                                                isOnMainLine={isOnMainLine}
                                            />
                                        </div>
                                    ) : null;

                                return (
                                    <div key={move.ply}>
                                        <div
                                            ref={
                                                isCurrent
                                                    ? (currentRowRef as React.RefObject<HTMLDivElement>)
                                                    : undefined
                                            }
                                            role="option"
                                            className={`${rowClassName} w-full text-left cursor-pointer hover:bg-accent/50`}
                                            onClick={handleRowClick}
                                            onKeyDown={(e) => {
                                                if (e.key === "Enter" || e.key === " ") {
                                                    e.preventDefault();
                                                    handleRowClick();
                                                }
                                            }}
                                            tabIndex={0}
                                        >
                                            {content}
                                        </div>
                                        {expandedDetails}
                                        {inlineBranches}
                                    </div>
                                );
                            })
                        )}
                    </div>
                )}
            </div>
        </TooltipProvider>
    );
}
