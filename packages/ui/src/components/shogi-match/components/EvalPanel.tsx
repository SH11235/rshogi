/**
 * 評価値パネル（折りたたみ可能）
 *
 * 評価値グラフを表示
 * 対局中のチート防止のためデフォルトで折りたたまれている
 */

import type { ReactElement } from "react";
import { useCallback, useState } from "react";
import type { EvalHistory } from "../utils/kifFormat";
import { EvalGraph } from "./EvalGraph";
import { EvalGraphModal } from "./EvalGraphModal";

interface EvalPanelProps {
    /** 評価値の履歴（グラフ用） */
    evalHistory: EvalHistory[];
    /** 現在の手数 */
    currentPly: number;
    /** 手数クリック時のコールバック */
    onPlySelect?: (ply: number) => void;
    /** デフォルトで開いているか */
    defaultOpen?: boolean;
}

/**
 * 評価値パネル
 * 評価値グラフを折りたたみ可能な形で表示
 */
export function EvalPanel({
    evalHistory,
    currentPly,
    onPlySelect,
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
        <div className="bg-card border border-border rounded-xl shadow-lg w-[var(--panel-width)] overflow-hidden">
            <button
                type="button"
                className={`flex justify-between items-center px-3 py-2.5 cursor-pointer select-none w-full bg-transparent border-0 text-left font-[inherit] text-[inherit] ${
                    isOpen ? "border-b border-border" : ""
                }`}
                onClick={handleToggle}
                aria-expanded={isOpen}
            >
                <div className="font-bold text-sm flex items-center gap-2">
                    <span>評価値グラフ</span>
                    {!isOpen && (
                        <span className="text-[11px] text-muted-foreground font-normal">
                            （クリックで展開）
                        </span>
                    )}
                </div>
                <span
                    className={`text-xs text-muted-foreground transition-transform duration-200 ${
                        isOpen ? "rotate-180" : "rotate-0"
                    }`}
                >
                    ▼
                </span>
            </button>

            {isOpen && (
                <div className="p-3">
                    {/* 評価値グラフ（クリックで拡大モーダル表示、手数選択対応） */}
                    <EvalGraph
                        evalHistory={evalHistory}
                        currentPly={currentPly}
                        compact={true}
                        height={80}
                        onClick={handleGraphClick}
                        onPlySelect={onPlySelect}
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
