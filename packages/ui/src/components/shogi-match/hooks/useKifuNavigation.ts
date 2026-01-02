/**
 * 棋譜ナビゲーションフック
 *
 * 分岐を含む棋譜のツリー構造を管理し、
 * 局面間のナビゲーション機能を提供する
 */

import type { AddMoveOptions, BoardState, PositionState } from "@shogi/app-core";
import {
    addMove as addMoveToTree,
    applyMoveWithState,
    createKifuTree,
    createPreferredPathCache,
    findNodeByPlyInCurrentPath,
    findNodeByPlyInMainLine,
    getBranchInfo,
    getCurrentNode,
    getMainLineMoves,
    getMovesToCurrent,
    goBack as goBackTree,
    goForward as goForwardTree,
    goToEnd as goToEndTree,
    goToNode,
    goToPly as goToPlyTree,
    goToStart as goToStartTree,
    isRewound as isRewoundTree,
    type KifuEval,
    type KifuTree,
    type PreferredPathCache,
    promoteToMainLine as promoteToMainLineTree,
    setNodeEval,
    switchBranch as switchBranchTree,
    truncateFromCurrent,
} from "@shogi/app-core";
import type { EngineInfoEvent } from "@shogi/engine-client";
import { useCallback, useMemo, useRef, useState } from "react";
import { normalizeEvalToSentePerspective } from "../utils/branchTreeUtils";
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
    /** 現在位置がメインライン上にあるか */
    isOnMainLine: boolean;
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
    /** 1手進む（優先分岐を指定可能） */
    goForward: (preferredBranchNodeId?: string) => void;
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
    /** 評価値を記録（手数で指定） */
    recordEvalByPly: (ply: number, event: EngineInfoEvent) => void;
    /** 評価値を記録（ノードIDで指定、分岐内のノード用） */
    recordEvalByNodeId: (nodeId: string, event: EngineInfoEvent) => void;
    /** PVを分岐として追加（onAddedは分岐が追加された場合にのみ呼ばれる） */
    addPvAsBranch: (
        ply: number,
        pv: string[],
        onAdded?: (info: { ply: number; firstMove: string }) => void,
    ) => void;
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
    /** 局面履歴を取得（各手が指された後の局面） */
    positionHistory: PositionState[];
    /** 分岐マーカー（ply -> 分岐数） */
    branchMarkers: Map<number, number>;
    /** 棋譜ツリー（高度な操作用） */
    tree: KifuTree;
    /** 指定ノードへジャンプ */
    goToNodeById: (nodeId: string) => void;
    /** 指定親ノードで分岐を切り替え */
    switchBranchAtNode: (parentNodeId: string, branchIndex: number) => void;
}

/**
 * 棋譜ナビゲーションフック
 */
export function useKifuNavigation(options: UseKifuNavigationOptions): UseKifuNavigationResult {
    const { initialPosition, initialSfen, onPositionChange } = options;

    // 棋譜ツリー
    const [tree, setTree] = useState<KifuTree>(() => createKifuTree(initialPosition, initialSfen));

    // 優先分岐パスのキャッシュ（goForwardのパフォーマンス改善用）
    // - refへの書き込みはレンダリング結果に影響しない副作用のため許容される
    // - ツリー変更操作（addMove, truncate, addPvAsBranch, reset）では明示的に無効化
    const pathCacheRef = useRef<PreferredPathCache | null>(null);

    /**
     * 1手進む
     * @param preferredBranchNodeId 優先する分岐のノードID（分岐ビューで使用）
     */
    const goForward = useCallback(
        (preferredBranchNodeId?: string) => {
            setTree((prev) => {
                // キャッシュの取得または作成
                // refへの書き込みはレンダリングに影響しないため許容
                let cache: PreferredPathCache | undefined;
                if (preferredBranchNodeId) {
                    const currentCache = pathCacheRef.current;
                    if (currentCache && currentCache.nodeId === preferredBranchNodeId) {
                        cache = currentCache;
                    } else {
                        cache = createPreferredPathCache(prev, preferredBranchNodeId);
                        pathCacheRef.current = cache;
                    }
                } else {
                    pathCacheRef.current = null;
                }

                const newTree = goForwardTree(prev, preferredBranchNodeId, cache);
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
     * 指定ノードへ直接ジャンプ
     */
    const goToNodeById = useCallback(
        (nodeId: string) => {
            pathCacheRef.current = null; // キャッシュを無効化
            setTree((prev) => {
                const newTree = goToNode(prev, nodeId);
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
     * 指定親ノードで分岐を切り替え
     * 親ノードへ移動してから指定インデックスの子ノードへ進む
     */
    const switchBranchAtNode = useCallback(
        (parentNodeId: string, branchIndex: number) => {
            pathCacheRef.current = null; // キャッシュを無効化
            setTree((prev) => {
                const parentNode = prev.nodes.get(parentNodeId);
                if (!parentNode || branchIndex < 0 || branchIndex >= parentNode.children.length) {
                    return prev;
                }
                const targetChildId = parentNode.children[branchIndex];
                const newTree = goToNode(prev, targetChildId);
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
     * 現在位置以降の手を削除
     */
    const truncate = useCallback(() => {
        pathCacheRef.current = null; // キャッシュを無効化
        setTree((prev) => truncateFromCurrent(prev));
    }, []);

    /**
     * 指し手を追加
     */
    const addMove = useCallback(
        (usiMove: string, positionAfter: PositionState, options?: AddMoveOptions) => {
            pathCacheRef.current = null; // キャッシュを無効化
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
     * 評価値を記録（手数で指定）
     * findNodeByPlyInCurrentPathを使用し、見つからなければfindNodeByPlyInMainLineで検索
     */
    const recordEvalByPly = useCallback((ply: number, event: EngineInfoEvent) => {
        setTree((prev) => {
            // 最適化: 現在位置からルートまで遡りながらplyに一致するノードを探す
            let nodeId = findNodeByPlyInCurrentPath(prev, ply);

            // 見つからなければメインラインから検索（現在位置より先のノードの場合）
            if (!nodeId) {
                nodeId = findNodeByPlyInMainLine(prev, ply);
            }

            if (nodeId) {
                const node = prev.nodes.get(nodeId);
                if (node) {
                    const evalData: KifuEval = {
                        scoreCp: event.scoreCp,
                        scoreMate: event.scoreMate,
                        depth: event.depth,
                        pv: event.pv,
                    };

                    const existing = node.eval;

                    // 更新条件:
                    // 1. 既存の評価値がない場合
                    // 2. 新しい探索深さが既存より深い場合
                    // 3. 既存にPVがなく、新しいデータにPVがある場合
                    const shouldUpdate =
                        !existing ||
                        (event.depth !== undefined && (existing.depth ?? 0) < event.depth) ||
                        (!existing.pv && event.pv && event.pv.length > 0);

                    if (shouldUpdate) {
                        return setNodeEval(prev, nodeId, evalData);
                    }
                }
            }

            return prev;
        });
    }, []);

    /**
     * ノードIDを指定して評価値を記録
     * 分岐内のノードなど、plyだけでは特定できないノードに評価値を保存する場合に使用
     */
    const recordEvalByNodeId = useCallback((nodeId: string, event: EngineInfoEvent) => {
        setTree((prev) => {
            const node = prev.nodes.get(nodeId);
            if (!node) return prev;

            const evalData: KifuEval = {
                scoreCp: event.scoreCp,
                scoreMate: event.scoreMate,
                depth: event.depth,
                pv: event.pv,
            };

            const existing = node.eval;
            const shouldUpdate =
                !existing ||
                (event.depth !== undefined && (existing.depth ?? 0) < event.depth) ||
                (!existing.pv && event.pv && event.pv.length > 0);

            if (shouldUpdate) {
                return setNodeEval(prev, nodeId, evalData);
            }

            return prev;
        });
    }, []);

    /**
     * PVを分岐として追加
     * 指定された手数のノードにPVを分岐として追加する
     * @param ply 分岐を追加する手数
     * @param pv PV（読み筋）の手順
     * @param onAdded 分岐が追加された場合に呼ばれるコールバック（ply, firstMoveを渡す）
     */
    const addPvAsBranch = useCallback(
        (
            ply: number,
            pv: string[],
            onAdded?: (info: { ply: number; firstMove: string }) => void,
        ) => {
            if (pv.length === 0) return;

            pathCacheRef.current = null; // キャッシュを無効化

            let branchAdded = false;
            const firstMove = pv[0];

            setTree((prev) => {
                // 指定plyのノードをメインラインから探す（本譜からの分岐のみサポート）
                const nodeId = findNodeByPlyInMainLine(prev, ply);
                if (!nodeId) {
                    return prev;
                }

                const node = prev.nodes.get(nodeId);
                if (!node) {
                    return prev;
                }

                // PVの最初の手が既存の子にあるか確認
                const existingChild = node.children
                    .map((id) => prev.nodes.get(id))
                    .find((child) => child?.usiMove === firstMove);

                if (existingChild) {
                    // 既に同じ手が存在する場合は何もしない
                    return prev;
                }

                // 分岐が成立するには、既存の子が1つ以上必要
                // （子が0の場合は単なるメインライン延長で、分岐ではない）
                const hadExistingChildren = node.children.length > 0;

                // 新しい分岐を追加
                let currentTree = goToNode(prev, nodeId);
                let currentPosition = node.positionAfter;
                let addedMoves = 0;

                for (const move of pv) {
                    const moveResult = applyMoveWithState(currentPosition, move, {
                        validateTurn: false,
                    });
                    if (!moveResult.ok) {
                        // 無効な手があれば終了
                        break;
                    }
                    currentTree = addMoveToTree(currentTree, move, moveResult.next);
                    currentPosition = moveResult.next;
                    addedMoves++;
                }

                // 元の位置に戻る
                const result = goToNode(currentTree, nodeId);

                // 分岐が成立したかどうかを記録
                // （既存の子があり、かつ新しい手が追加された場合のみ）
                if (addedMoves > 0 && hadExistingChildren) {
                    branchAdded = true;
                }

                return result;
            });

            // setTreeの外でコールバックをスケジュール
            // nodeIdではなくply + firstMoveを渡すことでStrictModeの影響を回避
            if (onAdded) {
                setTimeout(() => {
                    if (branchAdded) {
                        onAdded({ ply, firstMove });
                    }
                }, 0);
            }
        },
        [],
    );

    /**
     * リセット
     */
    const reset = useCallback(
        (startPosition: PositionState, startSfen: string) => {
            pathCacheRef.current = null; // キャッシュを無効化
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

        // 現在位置がメインライン上にあるかを判定
        // ルートから現在位置まで、各ノードが親の最初の子（children[0]）であればメインライン上
        let isOnMainLine = true;
        let checkNodeId: string | null = tree.currentNodeId;
        while (checkNodeId !== null && isOnMainLine) {
            const checkNode = tree.nodes.get(checkNodeId);
            if (!checkNode) break;
            if (checkNode.parentId !== null) {
                const parent = tree.nodes.get(checkNode.parentId);
                if (parent && parent.children[0] !== checkNodeId) {
                    isOnMainLine = false;
                }
            }
            checkNodeId = checkNode.parentId;
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
            isOnMainLine,
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

    // 局面履歴を計算（各手が指された後の局面、PV変換用）
    const positionHistory = useMemo((): PositionState[] => {
        const history: PositionState[] = [];

        for (const node of fullLinePath) {
            // 各手が指された後の局面（positionAfter）を記録
            if (node.ply > 0) {
                history.push(node.positionAfter);
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
            {
                scoreCp?: number;
                scoreMate?: number;
                depth?: number;
                elapsedMs?: number;
                pv?: string[];
            }
        >();

        for (const node of fullLinePath) {
            if (node.usiMove !== null) {
                moves.push(node.usiMove);
            }
            // 評価値と消費時間をまとめてマップに格納
            const hasEval = node.eval != null;
            const hasElapsed = node.elapsedMs != null;
            if (hasEval || hasElapsed) {
                const normalizedEval = normalizeEvalToSentePerspective(node.eval, node.ply);
                nodeDataMap.set(node.ply, {
                    scoreCp: normalizedEval.evalCp,
                    scoreMate: normalizedEval.evalMate,
                    depth: node.eval?.depth,
                    elapsedMs: node.elapsedMs,
                    pv: node.eval?.pv,
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

            const normalizedEval = normalizeEvalToSentePerspective(node.eval, node.ply);

            history.push({
                ply: node.ply,
                evalCp: normalizedEval.evalCp ?? null,
                evalMate: normalizedEval.evalMate ?? null,
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
        recordEvalByPly,
        recordEvalByNodeId,
        addPvAsBranch,
        reset,
        getMovesArray,
        getMainLineMoves: getMainLineMovesArray,
        kifMoves,
        evalHistory,
        boardHistory,
        positionHistory,
        branchMarkers,
        tree,
        goToNodeById,
        switchBranchAtNode,
    };
}
