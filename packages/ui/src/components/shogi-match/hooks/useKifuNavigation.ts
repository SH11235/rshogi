/**
 * 棋譜ナビゲーションフック
 *
 * 分岐を含む棋譜のツリー構造を管理し、
 * 局面間のナビゲーション機能を提供する
 */

import type { AddMoveOptions, BoardState, PositionState } from "@shogi/app-core";
import {
    addMove as addMoveToTree,
    createKifuTree,
    findNodeByPlyInCurrentPath,
    getBranchInfo,
    getCurrentNode,
    getMainLineMoves,
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

/** USI形式の指し手からlastMove情報を導出 */
function deriveLastMoveFromUsi(usiMove: string | null): { from?: string; to: string } | undefined {
    if (!usiMove) return undefined;
    // 駒打ち: "P*5e" のような形式
    if (usiMove.includes("*")) {
        const to = usiMove.slice(-2);
        return { to };
    }
    // 通常の移動: "7g7f" のような形式
    if (usiMove.length >= 4) {
        const from = usiMove.slice(0, 2);
        const to = usiMove.slice(2, 4);
        return { from, to };
    }
    return undefined;
}

/** ナビゲーション状態 */
interface KifuNavigationState {
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
    /** 進む操作が可能か（現在ノードに子がある） */
    canGoForward: boolean;
}

/** フックの初期化オプション */
interface UseKifuNavigationOptions {
    /** 開始局面 */
    initialPosition: PositionState;
    /** 開始局面のSFEN */
    initialSfen: string;
    /** 局面変更時のコールバック */
    onPositionChange?: (position: PositionState, lastMove?: { from?: string; to: string }) => void;
}

/** フックの戻り値 */
interface UseKifuNavigationResult {
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
    addMove: (usiMove: string, positionAfter: PositionState, options?: AddMoveOptions) => void;
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
    /** 分岐マーカー（ply -> 分岐数） */
    branchMarkers: Map<number, number>;
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
                const lastMove = deriveLastMoveFromUsi(node.usiMove);
                onPositionChange?.(node.positionAfter, lastMove);
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
                const lastMove = deriveLastMoveFromUsi(node.usiMove);
                onPositionChange?.(node.positionAfter, lastMove);
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
                const lastMove = deriveLastMoveFromUsi(node.usiMove);
                onPositionChange?.(node.positionAfter, lastMove);
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
                const lastMove = deriveLastMoveFromUsi(node.usiMove);
                onPositionChange?.(node.positionAfter, lastMove);
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
                    const lastMove = deriveLastMoveFromUsi(node.usiMove);
                    onPositionChange?.(node.positionAfter, lastMove);
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
                    const lastMove = deriveLastMoveFromUsi(node.usiMove);
                    onPositionChange?.(node.positionAfter, lastMove);
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
        (usiMove: string, positionAfter: PositionState, options?: AddMoveOptions) => {
            setTree((prev) => {
                const newTree = addMoveToTree(prev, usiMove, positionAfter, options);
                // 新しいノードに移動したので、コールバックを呼ぶ
                const lastMove = deriveLastMoveFromUsi(usiMove);
                onPositionChange?.(positionAfter, lastMove);
                return newTree;
            });
        },
        [onPositionChange],
    );

    /**
     * 評価値を記録
     * findNodeByPlyInCurrentPathを使用してO(depth)で効率的に検索
     */
    const recordEval = useCallback((ply: number, event: EngineInfoEvent) => {
        setTree((prev) => {
            // 最適化: 現在位置からルートまで遡りながらplyに一致するノードを探す
            const nodeId = findNodeByPlyInCurrentPath(prev, ply);
            if (nodeId) {
                const node = prev.nodes.get(nodeId);
                if (node) {
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

        // 現在ラインの終端plyを計算（children[0]を辿る）
        let endNode = currentNode;
        while (endNode.children.length > 0) {
            const nextNode = tree.nodes.get(endNode.children[0]);
            if (!nextNode) break;
            endNode = nextNode;
        }

        return {
            currentPly: currentNode.ply,
            currentNodeId: tree.currentNodeId,
            displayPosition: currentNode.positionAfter,
            totalPly: endNode.ply,
            hasBranches: branchInfo.hasBranches,
            currentBranchIndex: branchInfo.currentIndex,
            branchCount: branchInfo.count,
            isRewound: isRewoundTree(tree),
            canGoForward: currentNode.children.length > 0,
        };
    }, [tree]);

    // 現在位置までのノードパスを計算
    const currentLinePath = useMemo(() => {
        const path: typeof tree.nodes extends Map<string, infer N> ? N[] : never[] = [];
        let nodeId: string | null = tree.currentNodeId;

        // 現在位置からルートまで遡る
        while (nodeId !== null) {
            const node = tree.nodes.get(nodeId);
            if (!node) break;
            path.unshift(node);
            nodeId = node.parentId;
        }

        return path;
    }, [tree]);

    // 現在位置からライン終端までのフルパスを計算（巻き戻し時の未来の手も含む）
    const fullLinePath = useMemo(() => {
        // まず現在位置までのパスを取得
        const path = [...currentLinePath];

        // 現在位置から先（children[0]を辿る）を追加
        const currentNode = tree.nodes.get(tree.currentNodeId);
        if (currentNode && currentNode.children.length > 0) {
            let nodeId: string | null = currentNode.children[0];
            while (nodeId !== null) {
                const node = tree.nodes.get(nodeId);
                if (!node) break;
                path.push(node);
                nodeId = node.children.length > 0 ? node.children[0] : null;
            }
        }

        return path;
    }, [tree, currentLinePath]);

    // 盤面履歴を計算（フルラインから抽出、未来の手も含む）
    const boardHistory = useMemo((): BoardState[] => {
        const history: BoardState[] = [];

        for (const node of fullLinePath) {
            // ルート以外のノードについて、適用前の盤面を記録
            if (node.ply > 0) {
                history.push(node.boardBefore);
            }
        }

        return history;
    }, [fullLinePath]);

    // KIF形式の棋譜を生成（フルラインに対応、未来の手も含む）
    const kifMoves = useMemo((): KifMove[] => {
        // フルラインから指し手を抽出
        const moves: string[] = [];
        const nodeDataMap = new Map<
            number,
            { scoreCp?: number; scoreMate?: number; depth?: number; elapsedMs?: number }
        >();

        for (const node of fullLinePath) {
            if (node.usiMove !== null) {
                moves.push(node.usiMove);
            }
            // 評価値と消費時間をまとめてマップに格納
            const hasEval = node.eval != null;
            const hasElapsed = node.elapsedMs != null;
            if (hasEval || hasElapsed) {
                // エンジンの評価値は「手番側（次に指す側）から見た値」なので、
                // 先手の手の後（奇数手）は後手視点のため反転して先手視点に正規化
                const isSenteMove = node.ply % 2 !== 0;
                const sign = isSenteMove ? -1 : 1;
                nodeDataMap.set(node.ply, {
                    scoreCp: node.eval?.scoreCp != null ? node.eval.scoreCp * sign : undefined,
                    scoreMate:
                        node.eval?.scoreMate != null ? node.eval.scoreMate * sign : undefined,
                    depth: node.eval?.depth,
                    elapsedMs: node.elapsedMs,
                });
            }
        }

        if (moves.length === 0 || boardHistory.length === 0) {
            return [];
        }

        const validMoves = moves.slice(0, boardHistory.length);
        return convertMovesToKif(validMoves, boardHistory, nodeDataMap);
    }, [fullLinePath, boardHistory]);

    // 評価値履歴を生成（グラフ用、フルラインに対応、未来の手も含む）
    const evalHistory = useMemo((): EvalHistory[] => {
        const history: EvalHistory[] = [{ ply: 0, evalCp: 0, evalMate: null }];

        for (const node of fullLinePath) {
            // ルートはスキップ（ply: 0はすでに追加済み）
            if (node.ply === 0) continue;

            const ply = node.ply;
            const evalData = node.eval;

            // エンジンの評価値は「手番側（次に指す側）から見た値」なので、
            // 先手の手の後（奇数手）は後手視点のため反転して先手視点に正規化
            // 後手の手の後（偶数手）は先手視点のためそのまま
            const isSenteMove = ply % 2 !== 0;
            const sign = isSenteMove ? -1 : 1;

            history.push({
                ply,
                evalCp: evalData?.scoreCp != null ? evalData.scoreCp * sign : null,
                evalMate: evalData?.scoreMate != null ? evalData.scoreMate * sign : null,
            });
        }

        return history;
    }, [fullLinePath]);

    // 分岐マーカーを計算（フルラインで分岐がある手数とその分岐数）
    const branchMarkers = useMemo((): Map<number, number> => {
        const markers = new Map<number, number>();

        for (const node of fullLinePath) {
            // 子が2つ以上あれば分岐が存在
            if (node.children.length > 1) {
                markers.set(node.ply, node.children.length);
            }
        }

        return markers;
    }, [fullLinePath]);

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
        branchMarkers,
        tree,
    };
}
