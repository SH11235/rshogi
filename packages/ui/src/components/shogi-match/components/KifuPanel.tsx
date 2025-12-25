/**
 * KIF形式棋譜表示パネル
 *
 * 棋譜をKIF形式（日本語表記）で表示し、評価値も合わせて表示する
 */

import type { ReactElement } from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { KifMove } from "../utils/kifFormat";
import { formatEval } from "../utils/kifFormat";

interface KifuPanelProps {
    /** KIF形式の指し手リスト */
    kifMoves: KifMove[];
    /** 現在の手数（ハイライト用） */
    currentPly: number;
    /** 手数クリック時のコールバック（将来：局面ジャンプ用） */
    onPlySelect?: (ply: number) => void;
    /** 評価値を表示するか */
    showEval?: boolean;
    /** KIF形式でコピーするときのコールバック（KIF文字列を返す） */
    onCopyKif?: () => string;
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

            <div ref={listRef} className="max-h-60 overflow-auto my-2">
                {kifMoves.length === 0 ? (
                    <div className="text-[13px] text-muted-foreground text-center py-4">
                        まだ指し手がありません
                    </div>
                ) : (
                    kifMoves.map((move) => {
                        const isCurrent = move.ply === currentPly;
                        const evalText = showEval ? formatEval(move.evalCp, move.evalMate) : "";

                        const content = (
                            <>
                                <span className="text-right text-muted-foreground text-xs">
                                    {move.ply}
                                </span>
                                <span className="font-medium">{move.displayText}</span>
                                {showEval && evalText && (
                                    <span className={getEvalClassName(move.evalCp, move.evalMate)}>
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
