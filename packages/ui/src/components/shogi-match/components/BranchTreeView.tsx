/**
 * 分岐ツリービューコンポーネント
 *
 * 棋譜の分岐構造を視覚的に表示し、ナビゲーションを提供する
 */

import type { KifuTree } from "@shogi/app-core";
import type { ReactElement } from "react";
import { useCallback, useEffect, useMemo, useRef } from "react";
import { Tooltip, TooltipContent, TooltipTrigger } from "../../tooltip";
import type { BranchOption, FlatTreeNode } from "../utils/branchTreeUtils";
import { flattenTreeAlongCurrentPath } from "../utils/branchTreeUtils";
import { formatEval } from "../utils/kifFormat";

interface BranchTreeViewProps {
    /** 棋譜ツリー */
    tree: KifuTree;
    /** ノードクリック時のコールバック */
    onNodeClick?: (nodeId: string) => void;
    /** 分岐切り替え時のコールバック */
    onBranchSwitch?: (parentNodeId: string, branchIndex: number) => void;
    /** 評価値を表示するか */
    showEval?: boolean;
}

/**
 * 評価値のスタイルクラスを決定
 */
function getEvalClassName(evalCp?: number, evalMate?: number): string {
    const baseClass = "text-[10px] font-medium";
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
 * ノードマーカーコンポーネント
 */
function NodeMarker({
    isRoot,
    isCurrent,
    hasBranches,
}: {
    isRoot: boolean;
    isCurrent: boolean;
    hasBranches: boolean;
}): ReactElement {
    let className =
        "w-2.5 h-2.5 rounded-full flex-shrink-0 relative z-10 transition-all duration-150";

    if (isRoot) {
        className += " bg-[hsl(var(--wafuu-sumi))] border-2 border-[hsl(var(--wafuu-sumi))]";
    } else if (isCurrent) {
        className +=
            " bg-[hsl(var(--wafuu-kin))] border-2 border-[hsl(var(--wafuu-kin))] shadow-[0_0_8px_hsl(var(--wafuu-kin)/0.5)]";
    } else if (hasBranches) {
        className += " bg-[hsl(var(--wafuu-washi-warm))] border-2 border-[hsl(var(--wafuu-shu))]";
    } else {
        className +=
            " bg-[hsl(var(--wafuu-washi-warm))] border-2 border-[hsl(var(--wafuu-sumi-light))]";
    }

    return <span className={className} />;
}

/**
 * 分岐リストコンポーネント
 */
function BranchList({
    options,
    parentNodeId,
    onSelect,
}: {
    options: BranchOption[];
    parentNodeId: string;
    onSelect?: (parentNodeId: string, branchIndex: number) => void;
}): ReactElement {
    return (
        <div className="ml-6 mb-1 py-1.5 px-2 bg-[hsl(var(--wafuu-washi))] rounded-lg border-l-[3px] border-[hsl(var(--wafuu-shu))]">
            {options.map((opt) => (
                <button
                    key={opt.nodeId}
                    type="button"
                    onClick={() => onSelect?.(parentNodeId, opt.branchIndex)}
                    className={`
                        w-full flex items-center gap-2 px-2 py-1 rounded text-left
                        text-[12px] transition-all duration-150
                        ${
                            opt.isSelected
                                ? "bg-white text-[hsl(var(--wafuu-shu))] font-medium"
                                : "text-[hsl(var(--wafuu-sumi-light))] hover:bg-white hover:text-[hsl(var(--wafuu-sumi))]"
                        }
                    `}
                >
                    <span
                        className={`w-3 h-0.5 ${opt.isSelected ? "bg-[hsl(var(--wafuu-shu))]" : "bg-[hsl(var(--wafuu-shu)/0.4)]"}`}
                    />
                    <span className="flex-1">{opt.displayText}</span>
                    <span
                        className={`
                            text-[10px] px-1.5 py-0.5 rounded
                            ${
                                opt.isSelected
                                    ? "bg-[hsl(var(--wafuu-kin))] text-white"
                                    : "bg-[hsl(var(--wafuu-washi-warm))] text-[hsl(var(--wafuu-sumi-light))]"
                            }
                        `}
                    >
                        {opt.isMainLine ? "メイン" : `変化${opt.branchIndex}`}
                    </span>
                </button>
            ))}
        </div>
    );
}

/**
 * ツリーノードコンポーネント
 */
function TreeNode({
    node,
    showEval,
    isLast,
    onNodeClick,
    onBranchSwitch,
}: {
    node: FlatTreeNode;
    showEval: boolean;
    isLast: boolean;
    onNodeClick?: (nodeId: string) => void;
    onBranchSwitch?: (parentNodeId: string, branchIndex: number) => void;
}): ReactElement {
    const isRoot = node.ply === 0;
    const evalText = showEval ? formatEval(node.evalCp, node.evalMate, node.ply) : "";

    return (
        <div className="relative pl-6">
            {/* 縦のライン */}
            {!isLast && (
                <div
                    className="absolute left-[0.28rem] top-5 bottom-0 w-0.5 bg-gradient-to-b from-[hsl(var(--wafuu-sumi))] to-[hsl(var(--wafuu-sumi)/0.3)]"
                    aria-hidden="true"
                />
            )}

            {/* ノード本体 */}
            <button
                type="button"
                onClick={() => onNodeClick?.(node.nodeId)}
                className={`
                    flex items-center gap-2 w-full text-left
                    px-2 py-1 rounded-md transition-all duration-150
                    ${
                        node.isCurrent
                            ? "bg-gradient-to-r from-[hsl(var(--wafuu-kin)/0.15)] to-[hsl(var(--wafuu-kin)/0.08)] border border-[hsl(var(--wafuu-kin)/0.3)]"
                            : "hover:bg-[hsl(var(--wafuu-washi))]"
                    }
                `}
            >
                <NodeMarker
                    isRoot={isRoot}
                    isCurrent={node.isCurrent}
                    hasBranches={node.hasBranches}
                />

                {/* 手数 */}
                {!isRoot && (
                    <span className="text-[11px] text-muted-foreground min-w-[1.5rem] text-right">
                        {node.ply}
                    </span>
                )}

                {/* 指し手 */}
                <span
                    className={`flex-1 text-[13px] ${node.isCurrentPath ? "" : "text-muted-foreground"}`}
                >
                    {node.displayText}
                </span>

                {/* 分岐インジケーター */}
                {node.hasBranches && (
                    <Tooltip>
                        <TooltipTrigger asChild>
                            <span className="inline-flex items-center gap-1 text-[10px] text-[hsl(var(--wafuu-shu))] bg-[hsl(var(--wafuu-shu)/0.1)] px-1.5 py-0.5 rounded">
                                <span>◆</span>
                                <span>{node.branchOptions?.length ?? 0}</span>
                            </span>
                        </TooltipTrigger>
                        <TooltipContent side="right" className="text-[11px]">
                            {node.branchOptions?.length ?? 0}つの分岐
                        </TooltipContent>
                    </Tooltip>
                )}

                {/* 評価値 */}
                {evalText && (
                    <span className={getEvalClassName(node.evalCp, node.evalMate)}>{evalText}</span>
                )}
            </button>

            {/* 分岐リスト（展開時） */}
            {node.hasBranches && node.branchOptions && (
                <BranchList
                    options={node.branchOptions}
                    parentNodeId={node.nodeId}
                    onSelect={onBranchSwitch}
                />
            )}
        </div>
    );
}

/**
 * 分岐ツリービュー
 */
export function BranchTreeView({
    tree,
    onNodeClick,
    onBranchSwitch,
    showEval = true,
}: BranchTreeViewProps): ReactElement {
    const containerRef = useRef<HTMLDivElement>(null);
    const currentNodeRef = useRef<HTMLDivElement>(null);

    // ツリーをフラット化
    const flatNodes = useMemo(() => flattenTreeAlongCurrentPath(tree), [tree]);

    // 現在位置が変わったら自動スクロール
    useEffect(() => {
        const container = containerRef.current;
        if (!container) return;

        // 現在のノードを探す
        const currentIndex = flatNodes.findIndex((n) => n.isCurrent);
        if (currentIndex < 0) return;

        // スクロール位置を計算
        const nodeElements = container.querySelectorAll("[data-tree-node]");
        const currentElement = nodeElements[currentIndex] as HTMLElement | undefined;

        if (currentElement) {
            const containerRect = container.getBoundingClientRect();
            const elementRect = currentElement.getBoundingClientRect();
            const relativeTop = elementRect.top - containerRect.top + container.scrollTop;

            // 中央に配置
            const targetScrollTop =
                relativeTop - container.clientHeight / 2 + elementRect.height / 2;
            container.scrollTop = Math.max(0, targetScrollTop);
        }
    }, [flatNodes]);

    // ノードクリックハンドラ
    const handleNodeClick = useCallback(
        (nodeId: string) => {
            onNodeClick?.(nodeId);
        },
        [onNodeClick],
    );

    // 分岐切り替えハンドラ
    const handleBranchSwitch = useCallback(
        (parentNodeId: string, branchIndex: number) => {
            onBranchSwitch?.(parentNodeId, branchIndex);
        },
        [onBranchSwitch],
    );

    if (flatNodes.length === 0) {
        return (
            <div className="text-[13px] text-muted-foreground text-center py-4">
                棋譜がありません
            </div>
        );
    }

    return (
        <div ref={containerRef} className="flex-1 min-h-0 overflow-auto py-2">
            {flatNodes.map((node, index) => (
                <div
                    key={node.nodeId}
                    data-tree-node
                    ref={node.isCurrent ? currentNodeRef : undefined}
                >
                    <TreeNode
                        node={node}
                        showEval={showEval}
                        isLast={index === flatNodes.length - 1}
                        onNodeClick={handleNodeClick}
                        onBranchSwitch={handleBranchSwitch}
                    />
                </div>
            ))}
        </div>
    );
}
