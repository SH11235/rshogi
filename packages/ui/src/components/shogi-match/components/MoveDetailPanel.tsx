/**
 * 手の詳細パネルコンポーネント
 *
 * 棋譜パネルの右側に表示され、選択された手のMultiPV情報などを表示する
 */

import type { KifuTree, PositionState } from "@shogi/app-core";
import type { ReactElement } from "react";
import { useMemo } from "react";
import {
    comparePvWithMainLine,
    findExistingBranchForPv,
    type PvMainLineComparison,
} from "../utils/branchTreeUtils";
import type { KifMove, PvDisplayMove, PvEvalInfo } from "../utils/kifFormat";
import { convertPvToDisplay, formatEval, getEvalTooltipInfo } from "../utils/kifFormat";

interface MoveDetailPanelProps {
    /** 選択された手 */
    move: KifMove;
    /** 手が指された後の局面 */
    position: PositionState;
    /** PVを分岐として追加するコールバック */
    onAddBranch?: (ply: number, pv: string[]) => void;
    /** PVを盤面で確認するコールバック */
    onPreview?: (ply: number, pv: string[], evalCp?: number, evalMate?: number) => void;
    /** 指定手数の局面を解析するコールバック */
    onAnalyze?: (ply: number) => void;
    /** 解析中かどうか */
    isAnalyzing?: boolean;
    /** 現在解析中の手数 */
    analyzingPly?: number;
    /** 棋譜ツリー（分岐追加の重複チェック用） */
    kifuTree?: KifuTree;
    /** パネルを閉じるコールバック */
    onClose: () => void;
    /** 現在位置がメインライン上にあるか */
    isOnMainLine?: boolean;
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
 * 手の詳細パネル（右パネル表示用）
 */
export function MoveDetailPanel({
    move,
    position,
    onAddBranch,
    onPreview,
    onAnalyze,
    isAnalyzing,
    analyzingPly,
    kifuTree,
    onClose,
    isOnMainLine = true,
}: MoveDetailPanelProps): ReactElement {
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
        <div className="bg-card border border-border rounded-xl p-3 shadow-lg w-[var(--panel-width)]">
            {/* ヘッダー */}
            <div className="flex items-center justify-between mb-2 pb-2 border-b border-border">
                <div className="flex items-center gap-2">
                    <span className="font-bold">詳細</span>
                    <span className="text-[11px] text-muted-foreground">{move.ply}手目</span>
                    <span className="text-[13px] font-medium">{move.displayText}</span>
                </div>
                <button
                    type="button"
                    onClick={onClose}
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
                        width="16"
                        height="16"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="2"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        aria-hidden="true"
                    >
                        <line x1="18" y1="6" x2="6" y2="18" />
                        <line x1="6" y1="6" x2="18" y2="18" />
                    </svg>
                </button>
            </div>

            {/* 評価値サマリー */}
            <div className="flex items-center gap-2 mb-3 p-2 bg-[hsl(var(--wafuu-washi))] dark:bg-[hsl(var(--muted)/0.5)] rounded-lg">
                <span
                    className={`font-medium text-[14px] ${
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
                <div className="text-muted-foreground text-[10px] ml-auto space-x-1.5">
                    {evalInfo.detail && <span>{evalInfo.detail}</span>}
                    {evalInfo.depthText && <span>{evalInfo.depthText}</span>}
                </div>
            </div>

            {/* 複数PV候補リスト */}
            {hasPv && (
                <div className="space-y-2 max-h-[60vh] overflow-auto">
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

            {/* 読み筋もなく解析機能もない場合のメッセージ */}
            {!hasPv && !onAnalyze && (
                <div className="text-[12px] text-muted-foreground text-center py-4">
                    この手には詳細情報がありません
                </div>
            )}
        </div>
    );
}
