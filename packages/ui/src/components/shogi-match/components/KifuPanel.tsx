/**
 * KIF形式棋譜表示パネル
 *
 * 棋譜をKIF形式（日本語表記）で表示し、評価値も合わせて表示する
 */

import type { CSSProperties, ReactElement } from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { KifMove } from "../utils/kifFormat";
import { formatEval } from "../utils/kifFormat";

const baseCard: CSSProperties = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "12px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
    width: "var(--panel-width)",
};

const headerStyle: CSSProperties = {
    fontWeight: 700,
    marginBottom: "6px",
    display: "flex",
    justifyContent: "space-between",
    alignItems: "center",
    gap: "8px",
};

const headerLeftStyle: CSSProperties = {
    display: "flex",
    alignItems: "center",
    gap: "8px",
};

const copyButtonStyle: CSSProperties = {
    padding: "4px 8px",
    fontSize: "11px",
    borderRadius: "4px",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    background: "hsl(var(--background, 0 0% 100%))",
    cursor: "pointer",
    color: "hsl(var(--foreground, 0 0% 10%))",
    transition: "background 0.15s",
};

const copyButtonSuccessStyle: CSSProperties = {
    ...copyButtonStyle,
    background: "hsl(var(--success, 142 76% 36%))",
    color: "white",
    borderColor: "hsl(var(--success, 142 76% 36%))",
};

const moveCountStyle: CSSProperties = {
    fontSize: "13px",
    color: "hsl(var(--muted-foreground, 0 0% 48%))",
};

const moveListStyle: CSSProperties = {
    maxHeight: "240px",
    overflow: "auto",
    margin: "8px 0",
    padding: 0,
};

const moveRowStyle: CSSProperties = {
    display: "grid",
    gridTemplateColumns: "32px 1fr auto",
    gap: "4px",
    alignItems: "center",
    padding: "2px 4px",
    fontSize: "13px",
    fontFamily: "ui-monospace, monospace",
    borderRadius: "4px",
};

const moveRowCurrentStyle: CSSProperties = {
    ...moveRowStyle,
    background: "hsl(var(--accent, 210 40% 96%))",
};

const plyStyle: CSSProperties = {
    textAlign: "right",
    color: "hsl(var(--muted-foreground, 0 0% 48%))",
    fontSize: "12px",
};

const kifTextStyle: CSSProperties = {
    fontWeight: 500,
};

const evalStyle: CSSProperties = {
    fontSize: "11px",
    color: "hsl(var(--muted-foreground, 0 0% 48%))",
    textAlign: "right",
    minWidth: "48px",
};

const evalPositiveStyle: CSSProperties = {
    ...evalStyle,
    color: "hsl(var(--wafuu-shu, 350 80% 45%))",
};

const evalNegativeStyle: CSSProperties = {
    ...evalStyle,
    color: "hsl(210 70% 45%)",
};

const emptyMessageStyle: CSSProperties = {
    fontSize: "13px",
    color: "hsl(var(--muted-foreground, 0 0% 48%))",
    textAlign: "center",
    padding: "16px 0",
};

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
 * 評価値のスタイルを決定
 */
function getEvalStyle(evalCp?: number, evalMate?: number): CSSProperties {
    if (evalMate !== undefined && evalMate !== null) {
        return evalMate > 0 ? evalPositiveStyle : evalNegativeStyle;
    }
    if (evalCp !== undefined && evalCp !== null) {
        return evalCp >= 0 ? evalPositiveStyle : evalNegativeStyle;
    }
    return evalStyle;
}

export function KifuPanel({
    kifMoves,
    currentPly,
    onPlySelect,
    showEval = true,
    onCopyKif,
}: KifuPanelProps): ReactElement {
    const listRef = useRef<HTMLDivElement>(null);
    const currentRowRef = useRef<HTMLDivElement>(null);
    const [copySuccess, setCopySuccess] = useState(false);

    // 現在の手数が変わったら自動スクロール（コンテナ内のみ）
    useEffect(() => {
        if (currentRowRef.current && listRef.current) {
            const container = listRef.current;
            const row = currentRowRef.current;

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
        }
    }, [currentPly]);

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
        <div style={baseCard}>
            <div style={headerStyle}>
                <div style={headerLeftStyle}>
                    <span>棋譜</span>
                    <span style={moveCountStyle}>
                        {kifMoves.length === 0 ? "開始局面" : `${kifMoves.length}手`}
                    </span>
                </div>
                {onCopyKif && kifMoves.length > 0 && (
                    <button
                        type="button"
                        style={copySuccess ? copyButtonSuccessStyle : copyButtonStyle}
                        onClick={handleCopy}
                        title="KIF形式でクリップボードにコピー"
                    >
                        {copySuccess ? "コピー完了" : "KIFコピー"}
                    </button>
                )}
            </div>

            <div ref={listRef} style={moveListStyle}>
                {kifMoves.length === 0 ? (
                    <div style={emptyMessageStyle}>まだ指し手がありません</div>
                ) : (
                    kifMoves.map((move) => {
                        const isCurrent = move.ply === currentPly;
                        const evalText = showEval ? formatEval(move.evalCp, move.evalMate) : "";

                        return (
                            <div
                                key={move.ply}
                                ref={isCurrent ? currentRowRef : undefined}
                                style={isCurrent ? moveRowCurrentStyle : moveRowStyle}
                                onClick={() => onPlySelect?.(move.ply)}
                                role={onPlySelect ? "button" : undefined}
                                tabIndex={onPlySelect ? 0 : undefined}
                                onKeyDown={
                                    onPlySelect
                                        ? (e) => {
                                              if (e.key === "Enter" || e.key === " ") {
                                                  e.preventDefault();
                                                  onPlySelect(move.ply);
                                              }
                                          }
                                        : undefined
                                }
                            >
                                <span style={plyStyle}>{move.ply}</span>
                                <span style={kifTextStyle}>{move.kifText}</span>
                                {showEval && evalText && (
                                    <span style={getEvalStyle(move.evalCp, move.evalMate)}>
                                        {evalText}
                                    </span>
                                )}
                            </div>
                        );
                    })
                )}
            </div>
        </div>
    );
}
