/**
 * 評価値パネル（折りたたみ可能）
 *
 * 評価値グラフと棋譜（評価値付き）をまとめて表示
 * 対局中のチート防止のためデフォルトで折りたたまれている
 */

import type { CSSProperties, ReactElement } from "react";
import { useCallback, useState } from "react";
import type { EvalHistory, KifMove } from "../utils/kifFormat";
import { EvalGraph } from "./EvalGraph";
import { EvalGraphModal } from "./EvalGraphModal";
import { KifuPanel } from "./KifuPanel";

interface EvalPanelProps {
    /** 評価値の履歴（グラフ用） */
    evalHistory: EvalHistory[];
    /** KIF形式の指し手リスト */
    kifMoves: KifMove[];
    /** 現在の手数 */
    currentPly: number;
    /** 手数クリック時のコールバック */
    onPlySelect?: (ply: number) => void;
    /** KIFコピー用コールバック */
    onCopyKif?: () => string;
    /** デフォルトで開いているか */
    defaultOpen?: boolean;
}

const panelStyle: CSSProperties = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
    width: "var(--panel-width)",
    overflow: "hidden",
};

const headerStyle: CSSProperties = {
    display: "flex",
    justifyContent: "space-between",
    alignItems: "center",
    padding: "10px 12px",
    cursor: "pointer",
    userSelect: "none",
    borderBottom: "1px solid hsl(var(--border, 0 0% 86%))",
};

const headerCollapsedStyle: CSSProperties = {
    ...headerStyle,
    borderBottom: "none",
};

const titleStyle: CSSProperties = {
    fontWeight: 700,
    fontSize: "14px",
    display: "flex",
    alignItems: "center",
    gap: "8px",
};

const warningStyle: CSSProperties = {
    fontSize: "11px",
    color: "hsl(var(--muted-foreground, 0 0% 48%))",
    fontWeight: 400,
};

const toggleIconStyle: CSSProperties = {
    fontSize: "12px",
    color: "hsl(var(--muted-foreground, 0 0% 48%))",
    transition: "transform 0.2s ease",
};

const contentStyle: CSSProperties = {
    padding: "12px",
    display: "flex",
    flexDirection: "column",
    gap: "12px",
};

/**
 * 評価値パネル
 * 評価値グラフと棋譜を折りたたみ可能な形で表示
 */
export function EvalPanel({
    evalHistory,
    kifMoves,
    currentPly,
    onPlySelect,
    onCopyKif,
    defaultOpen = false,
}: EvalPanelProps): ReactElement {
    const [isOpen, setIsOpen] = useState(defaultOpen);
    const [showEvalModal, setShowEvalModal] = useState(false);

    const handleToggle = () => {
        setIsOpen(!isOpen);
    };

    const handleGraphClick = useCallback(() => {
        setShowEvalModal(true);
    }, []);

    const handleModalClose = useCallback(() => {
        setShowEvalModal(false);
    }, []);

    return (
        <div style={panelStyle}>
            <button
                type="button"
                style={isOpen ? headerStyle : headerCollapsedStyle}
                onClick={handleToggle}
                aria-expanded={isOpen}
            >
                <div style={titleStyle}>
                    <span>📊 評価値・解析</span>
                    {!isOpen && <span style={warningStyle}>（クリックで展開）</span>}
                </div>
                <span
                    style={{
                        ...toggleIconStyle,
                        transform: isOpen ? "rotate(180deg)" : "rotate(0deg)",
                    }}
                >
                    ▼
                </span>
            </button>

            {isOpen && (
                <div style={contentStyle}>
                    {/* 評価値グラフ（クリックで拡大モーダル表示） */}
                    <EvalGraph
                        evalHistory={evalHistory}
                        currentPly={currentPly}
                        compact={true}
                        height={80}
                        onClick={handleGraphClick}
                    />

                    {/* 棋譜パネル（評価値付き） */}
                    <KifuPanel
                        kifMoves={kifMoves}
                        currentPly={currentPly}
                        showEval={true}
                        onPlySelect={onPlySelect}
                        onCopyKif={onCopyKif}
                    />
                </div>
            )}

            {/* 評価値グラフ拡大モーダル */}
            <EvalGraphModal
                evalHistory={evalHistory}
                currentPly={currentPly}
                open={showEvalModal}
                onClose={handleModalClose}
            />
        </div>
    );
}
