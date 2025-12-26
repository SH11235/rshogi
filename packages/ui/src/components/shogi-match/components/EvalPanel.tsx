/**
 * 評価値パネル（折りたたみ可能）
 *
 * 評価値グラフを表示
 * 対局中のチート防止のためデフォルトで折りたたまれている
 */

import type { ReactElement } from "react";
import { useCallback, useState } from "react";
import { Button } from "../../button";
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
    /** 対局中かどうか（対局中は解析ボタンを無効化） */
    isMatchRunning?: boolean;
    /** 解析中かどうか */
    isAnalyzing?: boolean;
    /** 解析ボタンクリック時のコールバック */
    onAnalyze?: () => void;
    /** 解析キャンセルボタンクリック時のコールバック */
    onCancelAnalysis?: () => void;
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
    isMatchRunning = false,
    isAnalyzing = false,
    onAnalyze,
    onCancelAnalysis,
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

    // 現在の手数に評価値があるかどうか
    const currentEval = evalHistory.find((e) => e.ply === currentPly);
    const hasEval = currentEval?.evalCp !== null || currentEval?.evalMate !== null;

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

                    {/* 解析ボタン */}
                    {onAnalyze && (
                        <div className="mt-3 flex items-center gap-2">
                            {isAnalyzing ? (
                                <>
                                    <Button
                                        variant="outline"
                                        size="sm"
                                        onClick={onCancelAnalysis}
                                        className="flex-1"
                                    >
                                        解析中止
                                    </Button>
                                    <span className="text-xs text-muted-foreground animate-pulse">
                                        解析中...
                                    </span>
                                </>
                            ) : (
                                <Button
                                    variant="outline"
                                    size="sm"
                                    onClick={onAnalyze}
                                    disabled={isMatchRunning}
                                    className="flex-1"
                                    title={
                                        isMatchRunning
                                            ? "対局中は解析できません"
                                            : hasEval
                                              ? "現在の局面を再解析"
                                              : "現在の局面を解析"
                                    }
                                >
                                    {hasEval ? "再解析" : "解析"}
                                </Button>
                            )}
                        </div>
                    )}
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
