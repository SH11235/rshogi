/**
 * KIF形式棋譜表示パネル
 *
 * 棋譜をKIF形式（日本語表記）で表示し、評価値も合わせて表示する
 */

import type { ReactElement } from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { KifMove } from "../utils/kifFormat";
import { formatEval } from "../utils/kifFormat";
import { KifuNavigationToolbar } from "./KifuNavigationToolbar";

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
    /** 最大手数 */
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
    /** KIF形式でコピーするときのコールバック（KIF文字列を返す） */
    onCopyKif?: () => string;
    /** ナビゲーション機能（提供された場合はツールバーを表示） */
    navigation?: NavigationProps;
    /** ナビゲーション無効化（対局中など） */
    navigationDisabled?: boolean;
    /** 分岐マーカー（ply -> 分岐数） */
    branchMarkers?: Map<number, number>;
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
    onCopyKif,
    navigation,
    navigationDisabled = false,
    branchMarkers,
}: KifuPanelProps): ReactElement {
    const listRef = useRef<HTMLDivElement>(null);
    const currentRowRef = useRef<HTMLElement>(null);
    const [copySuccess, setCopySuccess] = useState(false);

    // 現在の手数が変わったら自動スクロール（コンテナ内のみ）
    useEffect(() => {
        // currentPlyが範囲外の場合はスクロールしない
        if (currentPly < 1 || currentPly > kifMoves.length) return;

        const container = listRef.current;
        const row = currentRowRef.current;
        if (!container || !row) return;

        // コンテナ内での相対位置を計算
        const rowTop = row.offsetTop - container.offsetTop;
        const rowBottom = rowTop + row.offsetHeight;
        const containerScrollTop = container.scrollTop;
        const containerHeight = container.clientHeight;

        // 行が表示範囲外にある場合のみスクロール（コンテナ内で）
        if (rowBottom > containerScrollTop + containerHeight) {
            // 行が下にはみ出ている
            container.scrollTop = rowBottom - containerHeight + 8;
        } else if (rowTop < containerScrollTop) {
            // 行が上にはみ出ている
            container.scrollTop = rowTop - 8;
        }
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
        <div className="bg-card border border-border rounded-xl p-3 shadow-lg w-[var(--panel-width)]">
            <div className="font-bold mb-1.5 flex justify-between items-center gap-2">
                <div className="flex items-center gap-2">
                    <span>棋譜</span>
                    <span className="text-[13px] text-muted-foreground">
                        {kifMoves.length === 0 ? "開始局面" : `${kifMoves.length}手`}
                    </span>
                </div>
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
                />
            )}

            <div ref={listRef} className="max-h-60 overflow-auto my-2">
                {kifMoves.length === 0 ? (
                    <div className="text-[13px] text-muted-foreground text-center py-4">
                        まだ指し手がありません
                    </div>
                ) : (
                    kifMoves.map((move) => {
                        const isCurrent = move.ply === currentPly;
                        const isPastCurrent = navigation?.isRewound && move.ply > currentPly;
                        const evalText = showEval ? formatEval(move.evalCp, move.evalMate) : "";
                        const hasBranch = branchMarkers?.has(move.ply);
                        const branchCount = branchMarkers?.get(move.ply);

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
                                {showEval && evalText && (
                                    <span
                                        className={`${getEvalClassName(move.evalCp, move.evalMate)} ${isPastCurrent ? "opacity-50" : ""}`}
                                    >
                                        {evalText}
                                    </span>
                                )}
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
    );
}
