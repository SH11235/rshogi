/**
 * 棋譜ナビゲーションフック
 *
 * 分岐を含む棋譜のツリー構造を管理し、
 * 局面間のナビゲーション機能を提供する
 */

import type { BoardState, PositionState } from "@shogi/app-core";
import {
    addMove as addMoveToTree,
    createKifuTree,
    getBranchInfo,
    getCurrentNode,
    getMainLineMoves,
    getMainLineTotalPly,
    getMovesToCurrent,
    goBack as goBackTree,
    goForward as goForwardTree,
    goToEnd as goToEndTree,
    goToPly as goToPlyTree,
    goToStart as goToStartTree,
    isRewound as isRewoundTree,
    type KifuEval,
    type KifuTree,
    promoteToMainLine as promoteToMainLineTree,
    setNodeEval,
    switchBranch as switchBranchTree,
    truncateFromCurrent,
} from "@shogi/app-core";
import type { EngineInfoEvent } from "@shogi/engine-client";
import { useCallback, useMemo, useState } from "react";
import type { EvalHistory, KifMove } from "../utils/kifFormat";
import { convertMovesToKif } from "../utils/kifFormat";

/** ナビゲーション状態 */
export interface KifuNavigationState {
    /** 現在の手数（0=開始局面） */
    currentPly: number;
    /** 現在のノードID */
    currentNodeId: string;
    /** 表示中の局面 */
    displayPosition: PositionState;
    /** 最新の手数（メインライン） */
    totalPly: number;
    /** 分岐が存在するか */
    hasBranches: boolean;
    /** 現在の分岐インデックス */
    currentBranchIndex: number;
    /** 利用可能な分岐数 */
    branchCount: number;
    /** 巻き戻し中か（currentPly < totalPly） */
    isRewound: boolean;
}

/** フックの初期化オプション */
export interface UseKifuNavigationOptions {
    /** 開始局面 */
    initialPosition: PositionState;
    /** 開始局面のSFEN */
    initialSfen: string;
    /** 局面変更時のコールバック */
    onPositionChange?: (position: PositionState, lastMove?: { from?: string; to: string }) => void;
}

/** フックの戻り値 */
export interface UseKifuNavigationResult {
    /** ナビゲーション状態 */
    state: KifuNavigationState;
    /** 1手進む */
    goForward: () => void;
    /** 1手戻る */
    goBack: () => void;
    /** 最初へ */
    goToStart: () => void;
    /** 最後へ（メインライン） */
    goToEnd: () => void;
    /** 指定手数へジャンプ */
    goToPly: (ply: number) => void;
    /** 分岐を切り替え */
    switchBranch: (index: number) => void;
    /** 現在の変化をメインに昇格 */
    promoteCurrentLine: () => void;
    /** 現在位置以降の手を削除 */
    truncate: () => void;
    /** 指し手を追加（分岐生成含む） */
    addMove: (usiMove: string, positionAfter: PositionState) => void;
    /** 評価値を記録 */
    recordEval: (ply: number, event: EngineInfoEvent) => void;
    /** 新規対局でリセット */
    reset: (startPosition: PositionState, startSfen: string) => void;
    /** 現在のラインの指し手配列を取得（互換性用） */
    getMovesArray: () => string[];
    /** メインラインの指し手配列を取得 */
    getMainLineMoves: () => string[];
    /** KIF形式の棋譜を取得 */
    kifMoves: KifMove[];
    /** 評価値履歴を取得（グラフ用） */
    evalHistory: EvalHistory[];
    /** 盤面履歴を取得 */
    boardHistory: BoardState[];
    /** 棋譜ツリー（高度な操作用） */
    tree: KifuTree;
}

/**
 * 棋譜ナビゲーションフック
 */
export function useKifuNavigation(options: UseKifuNavigationOptions): UseKifuNavigationResult {
    const { initialPosition, initialSfen, onPositionChange } = options;

    // 棋譜ツリー
    const [tree, setTree] = useState<KifuTree>(() => createKifuTree(initialPosition, initialSfen));

    /**
     * 1手進む
     */
    const goForward = useCallback(() => {
        setTree((prev) => {
            const newTree = goForwardTree(prev);
            if (newTree !== prev) {
                const node = getCurrentNode(newTree);
                onPositionChange?.(node.positionAfter);
            }
            return newTree;
        });
    }, [onPositionChange]);

    /**
     * 1手戻る
     */
    const goBack = useCallback(() => {
        setTree((prev) => {
            const newTree = goBackTree(prev);
            if (newTree !== prev) {
                const node = getCurrentNode(newTree);
                onPositionChange?.(node.positionAfter);
            }
            return newTree;
        });
    }, [onPositionChange]);

    /**
     * 最初へ
     */
    const goToStart = useCallback(() => {
        setTree((prev) => {
            const newTree = goToStartTree(prev);
            if (newTree !== prev) {
                const node = getCurrentNode(newTree);
                onPositionChange?.(node.positionAfter);
            }
            return newTree;
        });
    }, [onPositionChange]);

    /**
     * 最後へ（メインライン）
     */
    const goToEnd = useCallback(() => {
        setTree((prev) => {
            const newTree = goToEndTree(prev);
            if (newTree !== prev) {
                const node = getCurrentNode(newTree);
                onPositionChange?.(node.positionAfter);
            }
            return newTree;
        });
    }, [onPositionChange]);

    /**
     * 指定手数へジャンプ
     */
    const goToPly = useCallback(
        (ply: number) => {
            setTree((prev) => {
                const newTree = goToPlyTree(prev, ply);
                if (newTree !== prev) {
                    const node = getCurrentNode(newTree);
                    onPositionChange?.(node.positionAfter);
                }
                return newTree;
            });
        },
        [onPositionChange],
    );

    /**
     * 分岐を切り替え
     */
    const switchBranch = useCallback(
        (index: number) => {
            setTree((prev) => {
                const newTree = switchBranchTree(prev, index);
                if (newTree !== prev) {
                    const node = getCurrentNode(newTree);
                    onPositionChange?.(node.positionAfter);
                }
                return newTree;
            });
        },
        [onPositionChange],
    );

    /**
     * 現在の変化をメインに昇格
     */
    const promoteCurrentLine = useCallback(() => {
        setTree((prev) => promoteToMainLineTree(prev));
    }, []);

    /**
     * 現在位置以降の手を削除
     */
    const truncate = useCallback(() => {
        setTree((prev) => truncateFromCurrent(prev));
    }, []);

    /**
     * 指し手を追加
     */
    const addMove = useCallback(
        (usiMove: string, positionAfter: PositionState) => {
            setTree((prev) => {
                const newTree = addMoveToTree(prev, usiMove, positionAfter);
                // 新しいノードに移動したので、コールバックを呼ぶ
                onPositionChange?.(positionAfter);
                return newTree;
            });
        },
        [onPositionChange],
    );

    /**
     * 評価値を記録
     */
    const recordEval = useCallback((ply: number, event: EngineInfoEvent) => {
        setTree((prev) => {
            // plyに対応するノードを見つける
            // メインラインを辿ってplyに一致するノードを探す
            let nodeId = prev.rootId;
            let node = prev.nodes.get(nodeId);

            while (node && node.ply < ply && node.children.length > 0) {
                nodeId = node.children[0];
                node = prev.nodes.get(nodeId);
            }

            if (node && node.ply === ply) {
                const evalData: KifuEval = {
                    scoreCp: event.scoreCp,
                    scoreMate: event.scoreMate,
                    depth: event.depth,
                };

                // より深い探索深さの評価値で更新
                const existing = node.eval;
                if (
                    !existing ||
                    (event.depth !== undefined && (existing.depth ?? 0) < event.depth)
                ) {
                    return setNodeEval(prev, nodeId, evalData);
                }
            }

            return prev;
        });
    }, []);

    /**
     * リセット
     */
    const reset = useCallback(
        (startPosition: PositionState, startSfen: string) => {
            const newTree = createKifuTree(startPosition, startSfen);
            setTree(newTree);
            onPositionChange?.(startPosition);
        },
        [onPositionChange],
    );

    /**
     * 現在のラインの指し手配列を取得
     */
    const getMovesArray = useCallback(() => {
        return getMovesToCurrent(tree);
    }, [tree]);

    /**
     * メインラインの指し手配列を取得
     */
    const getMainLineMovesArray = useCallback(() => {
        return getMainLineMoves(tree);
    }, [tree]);

    // ナビゲーション状態を計算
    const state = useMemo((): KifuNavigationState => {
        const currentNode = getCurrentNode(tree);
        const branchInfo = getBranchInfo(tree);

        return {
            currentPly: currentNode.ply,
            currentNodeId: tree.currentNodeId,
            displayPosition: currentNode.positionAfter,
            totalPly: getMainLineTotalPly(tree),
            hasBranches: branchInfo.hasBranches,
            currentBranchIndex: branchInfo.currentIndex,
            branchCount: branchInfo.count,
            isRewound: isRewoundTree(tree),
        };
    }, [tree]);

    // 盤面履歴を計算（メインラインのノードから抽出）
    const boardHistory = useMemo((): BoardState[] => {
        const history: BoardState[] = [];
        let nodeId = tree.rootId;
        let node = tree.nodes.get(nodeId);

        while (node) {
            // ルート以外のノードについて、適用前の盤面を記録
            if (node.ply > 0) {
                history.push(node.boardBefore);
            }

            if (node.children.length > 0) {
                nodeId = node.children[0];
                node = tree.nodes.get(nodeId);
            } else {
                break;
            }
        }

        return history;
    }, [tree]);

    // KIF形式の棋譜を生成
    const kifMoves = useMemo((): KifMove[] => {
        const moves = getMainLineMoves(tree);
        if (moves.length === 0 || boardHistory.length === 0) {
            return [];
        }

        // 評価値マップを生成
        const evalMap = new Map<number, { scoreCp?: number; scoreMate?: number; depth?: number }>();
        let nodeId = tree.rootId;
        let node = tree.nodes.get(nodeId);

        while (node) {
            if (node.eval) {
                evalMap.set(node.ply, node.eval);
            }
            if (node.children.length > 0) {
                nodeId = node.children[0];
                node = tree.nodes.get(nodeId);
            } else {
                break;
            }
        }

        const validMoves = moves.slice(0, boardHistory.length);
        return convertMovesToKif(validMoves, boardHistory, evalMap);
    }, [tree, boardHistory]);

    // 評価値履歴を生成（グラフ用）
    const evalHistory = useMemo((): EvalHistory[] => {
        const history: EvalHistory[] = [{ ply: 0, evalCp: 0, evalMate: null }];

        let nodeId = tree.rootId;
        let node = tree.nodes.get(nodeId);

        while (node && node.children.length > 0) {
            nodeId = node.children[0];
            node = tree.nodes.get(nodeId);

            if (node) {
                const ply = node.ply;
                const evalData = node.eval;

                // エンジンの評価値は「指した側から見た値」なので、
                // 後手の手（偶数手）は符号を反転して先手視点に正規化
                const isGoteMove = ply % 2 === 0;
                const sign = isGoteMove ? -1 : 1;

                history.push({
                    ply,
                    evalCp: evalData?.scoreCp != null ? evalData.scoreCp * sign : null,
                    evalMate: evalData?.scoreMate != null ? evalData.scoreMate * sign : null,
                });
            }
        }

        return history;
    }, [tree]);

    return {
        state,
        goForward,
        goBack,
        goToStart,
        goToEnd,
        goToPly,
        switchBranch,
        promoteCurrentLine,
        truncate,
        addMove,
        recordEval,
        reset,
        getMovesArray,
        getMainLineMoves: getMainLineMovesArray,
        kifMoves,
        evalHistory,
        boardHistory,
        tree,
    };
}
