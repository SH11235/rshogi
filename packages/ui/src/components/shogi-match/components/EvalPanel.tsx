/**
 * 評価値パネル（折りたたみ可能）
 *
 * 評価値グラフと棋譜（評価値付き）をまとめて表示
 * 対局中のチート防止のためデフォルトで折りたたまれている
 */

import type { ReactElement } from "react";
import { useCallback, useState } from "react";
import type { EvalHistory, KifMove } from "../utils/kifFormat";
import { EvalGraph } from "./EvalGraph";
import { EvalGraphModal } from "./EvalGraphModal";
import { KifuPanel } from "./KifuPanel";

interface BranchInfo {
    hasBranches: boolean;
    currentIndex: number;
    count: number;
    onSwitch: (index: number) => void;
    onPromoteToMain?: () => void;
}

interface NavigationProps {
    /** 現在の手数（ナビゲーション用） */
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
    /** ナビゲーション機能 */
    navigation?: NavigationProps;
    /** ナビゲーション無効化（対局中など） */
    navigationDisabled?: boolean;
    /** 分岐マーカー（ply -> 分岐数） */
    branchMarkers?: Map<number, number>;
}

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
    navigation,
    navigationDisabled = false,
    branchMarkers,
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
                    <span>📊 評価値・解析</span>
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
                <div className="p-3 flex flex-col gap-3">
                    {/* 評価値グラフ（クリックで拡大モーダル表示、手数選択対応） */}
                    <EvalGraph
                        evalHistory={evalHistory}
                        currentPly={currentPly}
                        compact={true}
                        height={80}
                        onClick={handleGraphClick}
                        onPlySelect={onPlySelect}
                    />

                    {/* 棋譜パネル（評価値付き、ナビゲーション機能付き） */}
                    <KifuPanel
                        kifMoves={kifMoves}
                        currentPly={currentPly}
                        showEval={true}
                        onPlySelect={onPlySelect}
                        onCopyKif={onCopyKif}
                        navigation={navigation}
                        navigationDisabled={navigationDisabled}
                        branchMarkers={branchMarkers}
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
