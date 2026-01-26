/**
 * 棋譜ツリー管理モジュール
 *
 * 分岐を含む棋譜をツリー構造で管理し、ナビゲーション操作を提供する。
 */

import type { BoardState, PositionState } from "./board";
import { applyMoveWithState, cloneBoard } from "./board";

/** 評価値情報 */
export interface KifuEval {
    scoreCp?: number;
    scoreMate?: number;
    depth?: number;
    /**
     * 既に先手視点に正規化済みかどうか
     * - true: KIFインポートなど、既に先手視点の評価値
     * - false/undefined: エンジン出力（手番側視点）、符号反転が必要
     */
    normalized?: boolean;
    /** 読み筋（USI形式の指し手配列） */
    pv?: string[];
}

/** 棋譜ノード */
export interface KifuNode {
    /** ノードID（UUID） */
    id: string;
    /** USI形式の指し手（ルートノードはnull） */
    usiMove: string | null;
    /** 親ノードID（ルートはnull） */
    parentId: string | null;
    /** 子ノードID配列（分岐を保持、最初の要素がメインライン） */
    children: string[];
    /** 手数（ルート=0、最初の手=1） */
    ply: number;
    /** 指し手適用後の局面状態 */
    positionAfter: PositionState;
    /** 指し手適用前の盤面状態 */
    boardBefore: BoardState;
    /** 評価値情報（後方互換性のため残す、multipv=1相当） */
    eval?: KifuEval;
    /**
     * 複数PV用の評価値配列（multipv順、1-indexed相当）
     * - multiPvEvals[0] = multipv=1 の評価値
     * - multiPvEvals[1] = multipv=2 の評価値
     * - 未設定の位置は undefined になる可能性がある
     */
    multiPvEvals?: (KifuEval | undefined)[];
    /** コメント（オプション） */
    comment?: string;
    /** 消費時間（ミリ秒） */
    elapsedMs?: number;
}

/** 棋譜ツリー */
export interface KifuTree {
    /** ルートノードID */
    rootId: string;
    /** 全ノードのマップ */
    nodes: Map<string, KifuNode>;
    /** 現在位置のノードID */
    currentNodeId: string;
    /** 開始局面（SFEN形式） */
    startSfen: string;
}

/**
 * 優先分岐パスのキャッシュ
 * goForward関数のパフォーマンス改善のため、preferredBranchNodeIdからパスへのマッピングをキャッシュ
 */
export interface PreferredPathCache {
    /** キャッシュされているpreferredBranchNodeId */
    nodeId: string;
    /** ルートからpreferredBranchNodeIdまでのパス（Set形式） */
    pathSet: Set<string>;
}

/** UUID生成 */
function generateId(): string {
    return crypto.randomUUID();
}

/**
 * 新規棋譜ツリーを作成
 */
export function createKifuTree(startPosition: PositionState, startSfen: string): KifuTree {
    const rootId = generateId();
    const rootNode: KifuNode = {
        id: rootId,
        usiMove: null,
        parentId: null,
        children: [],
        ply: 0,
        positionAfter: startPosition,
        boardBefore: startPosition.board,
    };

    const nodes = new Map<string, KifuNode>();
    nodes.set(rootId, rootNode);

    return {
        rootId,
        nodes,
        currentNodeId: rootId,
        startSfen,
    };
}

/**
 * ノードを取得（存在しない場合はundefined）
 */
function getNode(tree: KifuTree, nodeId: string): KifuNode | undefined {
    return tree.nodes.get(nodeId);
}

/**
 * 現在のノードを取得
 */
export function getCurrentNode(tree: KifuTree): KifuNode {
    const node = tree.nodes.get(tree.currentNodeId);
    if (!node) {
        throw new Error(`Current node not found: ${tree.currentNodeId}`);
    }
    return node;
}

/**
 * 現在位置に手を追加
 * 既に同じ手が子として存在する場合はそのノードに移動
 * 新しい手の場合は分岐として追加
 */
/** addMoveのオプション */
export interface AddMoveOptions {
    /** 消費時間（ミリ秒） */
    elapsedMs?: number;
    /** 評価値情報 */
    eval?: KifuEval;
}

export function addMove(
    tree: KifuTree,
    usiMove: string,
    positionAfter: PositionState,
    options?: AddMoveOptions,
): KifuTree {
    const currentNode = getCurrentNode(tree);

    // 既存の子ノードに同じ手がないか確認
    const existingChild = currentNode.children
        .map((childId) => tree.nodes.get(childId))
        .find((child) => child?.usiMove === usiMove);

    if (existingChild) {
        // 既存のノードに移動
        return {
            ...tree,
            currentNodeId: existingChild.id,
        };
    }

    // 新しいノードを作成
    const newNodeId = generateId();
    const newNode: KifuNode = {
        id: newNodeId,
        usiMove,
        parentId: currentNode.id,
        children: [],
        ply: currentNode.ply + 1,
        positionAfter,
        boardBefore: cloneBoard(currentNode.positionAfter.board),
        elapsedMs: options?.elapsedMs,
        eval: options?.eval,
    };

    // ノードマップを更新
    const newNodes = new Map(tree.nodes);
    newNodes.set(newNodeId, newNode);

    // 親ノードの子リストを更新
    const updatedParent: KifuNode = {
        ...currentNode,
        children: [...currentNode.children, newNodeId],
    };
    newNodes.set(currentNode.id, updatedParent);

    return {
        ...tree,
        nodes: newNodes,
        currentNodeId: newNodeId,
    };
}

/**
 * 指定ノードに移動
 */
export function goToNode(tree: KifuTree, nodeId: string): KifuTree {
    if (!tree.nodes.has(nodeId)) {
        return tree; // ノードが存在しない場合は変更なし
    }
    return {
        ...tree,
        currentNodeId: nodeId,
    };
}

/**
 * 1手進む
 *
 * @param tree 棋譜ツリー
 * @param preferredBranchNodeId 優先する分岐のノードID（分岐ビュー用）
 *   - 指定された場合、その分岐への経路上にあれば分岐方向へ進む
 *   - 指定されていないか、経路上にない場合はメインライン（children[0]）へ進む
 * @param pathCache オプションのパスキャッシュ（パフォーマンス改善用）
 */
export function goForward(
    tree: KifuTree,
    preferredBranchNodeId?: string,
    pathCache?: PreferredPathCache,
): KifuTree {
    const currentNode = getCurrentNode(tree);
    if (currentNode.children.length === 0) {
        return tree; // 子がない場合は変更なし
    }

    // 優先分岐が指定されている場合、その分岐への経路を確認
    if (preferredBranchNodeId) {
        // キャッシュがあり、同じpreferredBranchNodeIdの場合はキャッシュを使用
        let pathSet: Set<string>;
        if (pathCache && pathCache.nodeId === preferredBranchNodeId) {
            pathSet = pathCache.pathSet;
        } else {
            // キャッシュがない場合はパスを計算
            const pathToPreferred = getPathToNode(tree, preferredBranchNodeId);
            pathSet = new Set(pathToPreferred);
        }

        // 現在のノードがパス上にあるか確認
        if (pathSet.has(currentNode.id)) {
            // パス上の次のノード（現在ノードの子の中でパスに含まれるもの）を探す
            for (const childId of currentNode.children) {
                if (pathSet.has(childId)) {
                    return {
                        ...tree,
                        currentNodeId: childId,
                    };
                }
            }
        }

        // 現在位置がpreferredBranchNodeIdの子孫（分岐内）にいる場合は、
        // 分岐に沿って進む（children[0]）
        if (isDescendantOf(tree, currentNode.id, preferredBranchNodeId)) {
            return {
                ...tree,
                currentNodeId: currentNode.children[0],
            };
        }
    }

    // デフォルト: メインライン（最初の子）へ進む
    return {
        ...tree,
        currentNodeId: currentNode.children[0],
    };
}

/**
 * 優先分岐パスのキャッシュを作成する
 * goForward関数と組み合わせて使用し、同じpreferredBranchNodeIdで複数回進む場合のパフォーマンスを改善
 *
 * @param tree 棋譜ツリー
 * @param preferredBranchNodeId 優先分岐のノードID
 * @returns パスキャッシュ
 */
export function createPreferredPathCache(
    tree: KifuTree,
    preferredBranchNodeId: string,
): PreferredPathCache {
    const pathToPreferred = getPathToNode(tree, preferredBranchNodeId);
    return {
        nodeId: preferredBranchNodeId,
        pathSet: new Set(pathToPreferred),
    };
}

/**
 * nodeIdがancestorIdの子孫（またはancestorId自身）かどうかを判定
 */
function isDescendantOf(tree: KifuTree, nodeId: string, ancestorId: string): boolean {
    const visited = new Set<string>();
    let currentId: string | null = nodeId;
    while (currentId !== null) {
        if (visited.has(currentId)) {
            // 循環参照を検出
            return false;
        }
        if (currentId === ancestorId) return true;
        visited.add(currentId);
        const node = tree.nodes.get(currentId);
        if (!node) break;
        currentId = node.parentId;
    }
    return false;
}

/**
 * 1手戻る（親ノードに移動）
 */
export function goBack(tree: KifuTree): KifuTree {
    const currentNode = getCurrentNode(tree);
    if (currentNode.parentId === null) {
        return tree; // ルートの場合は変更なし
    }
    return {
        ...tree,
        currentNodeId: currentNode.parentId,
    };
}

/**
 * 最初に戻る（ルートノードに移動）
 */
export function goToStart(tree: KifuTree): KifuTree {
    return {
        ...tree,
        currentNodeId: tree.rootId,
    };
}

/**
 * 最後に進む（現在のラインの末端まで移動）
 */
export function goToEnd(tree: KifuTree): KifuTree {
    let currentId = tree.currentNodeId;
    let node = tree.nodes.get(currentId);

    while (node && node.children.length > 0) {
        currentId = node.children[0]; // メインラインを辿る
        node = tree.nodes.get(currentId);
    }

    return {
        ...tree,
        currentNodeId: currentId,
    };
}

/**
 * 指定手数に移動（現在のラインを基準）
 *
 * - targetPly < currentPly: 親を辿って戻る
 * - targetPly > currentPly: 子を辿って進む（最初の子を選択）
 * - targetPly == currentPly: 変更なし
 */
export function goToPly(tree: KifuTree, targetPly: number): KifuTree {
    if (targetPly < 0) {
        return tree;
    }

    const currentNode = getCurrentNode(tree);
    const currentPly = currentNode.ply;

    if (targetPly === currentPly) {
        return tree;
    }

    if (targetPly < currentPly) {
        // 戻る: 親を辿る
        let nodeId = tree.currentNodeId;
        let node = tree.nodes.get(nodeId);

        while (node && node.ply > targetPly) {
            if (node.parentId === null) break;
            nodeId = node.parentId;
            node = tree.nodes.get(nodeId);
        }

        return {
            ...tree,
            currentNodeId: nodeId,
        };
    }

    // 進む: 子を辿る（現在のラインに沿って）
    let nodeId = tree.currentNodeId;
    let node = tree.nodes.get(nodeId);

    while (node && node.ply < targetPly && node.children.length > 0) {
        nodeId = node.children[0]; // 現在のラインの最初の子を辿る
        node = tree.nodes.get(nodeId);
    }

    return {
        ...tree,
        currentNodeId: nodeId,
    };
}

/**
 * 分岐を切り替え（現在のノードの親の子リストから選択）
 */
export function switchBranch(tree: KifuTree, branchIndex: number): KifuTree {
    const currentNode = getCurrentNode(tree);
    if (currentNode.parentId === null) {
        return tree; // ルートには分岐がない
    }

    const parentNode = tree.nodes.get(currentNode.parentId);
    if (!parentNode || branchIndex < 0 || branchIndex >= parentNode.children.length) {
        return tree;
    }

    return {
        ...tree,
        currentNodeId: parentNode.children[branchIndex],
    };
}

/**
 * 現在の変化をメインラインに昇格
 * 現在のノードを親の子リストの先頭に移動
 */
export function promoteToMainLine(tree: KifuTree): KifuTree {
    const currentNode = getCurrentNode(tree);
    if (currentNode.parentId === null) {
        return tree; // ルートには分岐がない
    }

    const parentNode = tree.nodes.get(currentNode.parentId);
    if (!parentNode) {
        return tree;
    }

    const currentIndex = parentNode.children.indexOf(currentNode.id);
    if (currentIndex <= 0) {
        return tree; // 既にメインライン
    }

    // 現在のノードを先頭に移動
    const newChildren = [
        currentNode.id,
        ...parentNode.children.filter((id) => id !== currentNode.id),
    ];

    const newNodes = new Map(tree.nodes);
    newNodes.set(parentNode.id, {
        ...parentNode,
        children: newChildren,
    });

    return {
        ...tree,
        nodes: newNodes,
    };
}

/**
 * 現在位置からの手を削除（分岐として残さない場合）
 * 現在のノードの子をすべて削除
 */
export function truncateFromCurrent(tree: KifuTree): KifuTree {
    const currentNode = getCurrentNode(tree);
    if (currentNode.children.length === 0) {
        return tree;
    }

    // 再帰的に子孫ノードを収集
    const toDelete = new Set<string>();
    const collectDescendants = (nodeId: string) => {
        const node = tree.nodes.get(nodeId);
        if (node) {
            for (const childId of node.children) {
                toDelete.add(childId);
                collectDescendants(childId);
            }
        }
    };

    for (const childId of currentNode.children) {
        toDelete.add(childId);
        collectDescendants(childId);
    }

    // 新しいノードマップを作成（削除対象を除外）
    const newNodes = new Map<string, KifuNode>();
    for (const [id, node] of tree.nodes) {
        if (!toDelete.has(id)) {
            if (id === currentNode.id) {
                // 現在ノードの子リストをクリア
                newNodes.set(id, { ...node, children: [] });
            } else {
                newNodes.set(id, node);
            }
        }
    }

    return {
        ...tree,
        nodes: newNodes,
    };
}

/**
 * ツリーから現在のラインの指し手配列を取得
 * ルートから現在位置までの手を配列として返す
 */
export function getMovesToCurrent(tree: KifuTree): string[] {
    const moves: string[] = [];
    let nodeId: string | null = tree.currentNodeId;

    // 現在位置からルートまで遡る
    const path: KifuNode[] = [];
    while (nodeId !== null) {
        const node = tree.nodes.get(nodeId);
        if (!node) break;
        path.unshift(node);
        nodeId = node.parentId;
    }

    // ルートを除いて手を収集
    for (const node of path) {
        if (node.usiMove !== null) {
            moves.push(node.usiMove);
        }
    }

    return moves;
}

/**
 * メインラインの指し手配列を取得
 * ルートから末端までメインライン（各ノードの最初の子）を辿る
 */
export function getMainLineMoves(tree: KifuTree): string[] {
    const moves: string[] = [];
    let nodeId: string | null = tree.rootId;

    while (nodeId !== null) {
        const node = tree.nodes.get(nodeId);
        if (!node) break;

        if (node.usiMove !== null) {
            moves.push(node.usiMove);
        }

        if (node.children.length > 0) {
            nodeId = node.children[0];
        } else {
            break;
        }
    }

    return moves;
}

/**
 * 現在位置に分岐があるか
 */
export function hasBranchAtCurrent(tree: KifuTree): boolean {
    const currentNode = getCurrentNode(tree);
    if (currentNode.parentId === null) {
        return false;
    }
    const parentNode = tree.nodes.get(currentNode.parentId);
    return parentNode !== undefined && parentNode.children.length > 1;
}

/**
 * 現在位置の分岐情報を取得
 */
export function getBranchInfo(tree: KifuTree): {
    hasBranches: boolean;
    currentIndex: number;
    count: number;
    siblings: KifuNode[];
} {
    const currentNode = getCurrentNode(tree);
    if (currentNode.parentId === null) {
        return { hasBranches: false, currentIndex: 0, count: 1, siblings: [currentNode] };
    }

    const parentNode = tree.nodes.get(currentNode.parentId);
    if (!parentNode) {
        return { hasBranches: false, currentIndex: 0, count: 1, siblings: [currentNode] };
    }

    const siblings = parentNode.children
        .map((id) => tree.nodes.get(id))
        .filter((n): n is KifuNode => n !== undefined);

    return {
        hasBranches: siblings.length > 1,
        currentIndex: parentNode.children.indexOf(currentNode.id),
        count: siblings.length,
        siblings,
    };
}

/**
 * ノードに評価値を設定
 */
export function setNodeEval(tree: KifuTree, nodeId: string, evalData: KifuEval): KifuTree {
    const node = tree.nodes.get(nodeId);
    if (!node) {
        return tree;
    }

    const newNodes = new Map(tree.nodes);
    newNodes.set(nodeId, {
        ...node,
        eval: evalData,
    });

    return {
        ...tree,
        nodes: newNodes,
    };
}

/**
 * ノードにコメントを設定
 */
export function setNodeComment(tree: KifuTree, nodeId: string, comment: string): KifuTree {
    const node = tree.nodes.get(nodeId);
    if (!node) {
        return tree;
    }

    const newNodes = new Map(tree.nodes);
    newNodes.set(nodeId, {
        ...node,
        comment,
    });

    return {
        ...tree,
        nodes: newNodes,
    };
}

/**
 * ノードに複数PVの評価値を設定
 *
 * multiPvEvals配列はmultipv順（1-indexed相当）で格納される:
 * - multiPvEvals[0] = multipv=1 の評価値
 * - multiPvEvals[1] = multipv=2 の評価値
 * - 未設定の位置は undefined になる（スパース配列）
 *
 * @param tree 棋譜ツリー
 * @param nodeId ノードID
 * @param multipv PV番号（1-indexed）
 * @param evalData 評価値データ
 * @returns 更新された棋譜ツリー
 */
export function setNodeMultiPvEval(
    tree: KifuTree,
    nodeId: string,
    multipv: number,
    evalData: KifuEval,
): KifuTree {
    // multipv は 1 以上の正の整数である必要がある
    if (multipv < 1 || !Number.isInteger(multipv)) {
        return tree;
    }

    const node = tree.nodes.get(nodeId);
    if (!node) {
        return tree;
    }

    // multipv に対応するインデックス（1-indexed → 0-indexed）
    const index = multipv - 1;

    // 既存の配列をコピー（スパース配列を維持）
    const newLength = Math.max(node.multiPvEvals?.length ?? 0, multipv);
    const newEvals: (KifuEval | undefined)[] = new Array(newLength);

    // 既存の値をコピー
    if (node.multiPvEvals) {
        for (let i = 0; i < node.multiPvEvals.length; i++) {
            newEvals[i] = node.multiPvEvals[i];
        }
    }

    // 既存エントリがある場合は深さを比較して更新判定
    const existing = newEvals[index];
    if (existing) {
        // 両方に深さがある場合
        if (evalData.depth !== undefined && existing.depth !== undefined) {
            if (existing.depth > evalData.depth) {
                // 既存のほうが深い場合はスキップ
                return tree;
            }
            if (existing.depth === evalData.depth) {
                // 同じ深さの場合、新しいPVがあり既存のPVが空なら更新を許可
                const existingHasPv = existing.pv && existing.pv.length > 0;
                const newHasPv = evalData.pv && evalData.pv.length > 0;
                if (existingHasPv || !newHasPv) {
                    // 既存にPVがある、または新しいデータにもPVがない場合はスキップ
                    return tree;
                }
                // 新しいデータにPVがあり既存にPVがない場合は更新を許可
            }
            // 新しいほうが深い場合は更新を許可
        } else if (evalData.depth === undefined && existing.depth !== undefined) {
            // 新規に深さ情報がなく、既存にある場合はスキップ
            // （深さ情報がない新しいデータで上書きしない）
            return tree;
        }
        // それ以外（新規に深さがあり既存にない、または両方に深さがない）は更新を許可
    }

    // 評価値を設定
    newEvals[index] = evalData;

    const newNodes = new Map(tree.nodes);
    newNodes.set(nodeId, {
        ...node,
        multiPvEvals: newEvals,
    });

    return {
        ...tree,
        nodes: newNodes,
    };
}

/**
 * メインラインの総手数を取得
 */
export function getMainLineTotalPly(tree: KifuTree): number {
    let maxPly = 0;
    let nodeId: string | null = tree.rootId;

    while (nodeId !== null) {
        const node = tree.nodes.get(nodeId);
        if (!node) break;

        maxPly = node.ply;

        if (node.children.length > 0) {
            nodeId = node.children[0];
        } else {
            break;
        }
    }

    return maxPly;
}

/**
 * 現在位置が巻き戻し中かどうか
 * 現在のラインで進める手がある場合true（現在ノードに子がある）
 */
export function isRewound(tree: KifuTree): boolean {
    const currentNode = getCurrentNode(tree);
    return currentNode.children.length > 0;
}

/**
 * ルートから指定ノードまでのパス（ノードID配列）を取得
 */
export function getPathToNode(tree: KifuTree, nodeId: string): string[] {
    const path: string[] = [];
    const visited = new Set<string>();
    let currentId: string | null = nodeId;

    while (currentId !== null) {
        if (visited.has(currentId)) {
            // 循環参照を検出した場合は現在のパスを返す
            break;
        }
        visited.add(currentId);
        path.unshift(currentId);
        const node = tree.nodes.get(currentId);
        if (!node) break;
        currentId = node.parentId;
    }

    return path;
}

/**
 * 現在位置からルートまでのパスを辿り、指定plyに一致するノードIDを探す
 * O(depth)で効率的に検索
 */
export function findNodeByPlyInCurrentPath(tree: KifuTree, ply: number): string | null {
    let nodeId: string | null = tree.currentNodeId;

    // 現在位置からルートまで遡りながらplyに一致するノードを探す
    while (nodeId !== null) {
        const node = tree.nodes.get(nodeId);
        if (!node) break;

        if (node.ply === ply) {
            return nodeId;
        }

        // 目的のplyより小さくなったら、見つからない
        if (node.ply < ply) {
            break;
        }

        nodeId = node.parentId;
    }

    return null;
}

/**
 * メインラインを辿り、指定plyに一致するノードIDを探す
 * ルートから順にchildren[0]を辿って検索
 * O(ply)で検索
 */
export function findNodeByPlyInMainLine(tree: KifuTree, ply: number): string | null {
    // ply 0 はルートノード
    if (ply === 0) {
        return tree.rootId;
    }

    let nodeId: string | null = tree.rootId;
    let currentPly = 0;

    // ルートからchildren[0]を辿ってplyまで進む
    while (nodeId !== null && currentPly < ply) {
        const node = tree.nodes.get(nodeId);
        if (!node) break;

        // 次の子ノードへ
        if (node.children.length === 0) {
            // これ以上進めない
            return null;
        }
        nodeId = node.children[0];
        currentPly++;
    }

    // 目的のplyに到達したか確認
    if (nodeId) {
        const node = tree.nodes.get(nodeId);
        if (node && node.ply === ply) {
            return nodeId;
        }
    }

    return null;
}

/** addMovesSilentlyの結果型 */
interface AddMovesSilentlyResult {
    tree: KifuTree;
    success: boolean;
    failedAt?: number;
}

/**
 * 複数の指し手を一括で追加（既存の棋譜をインポートする場合など）
 */
function addMovesSilently(
    tree: KifuTree,
    moves: string[],
    initialPosition: PositionState,
): AddMovesSilentlyResult {
    let currentTree = tree;
    let position = initialPosition;

    for (let i = 0; i < moves.length; i++) {
        const move = moves[i];
        const result = applyMoveWithState(position, move, { validateTurn: false });
        if (!result.ok) {
            return { tree: currentTree, success: false, failedAt: i };
        }
        currentTree = addMove(currentTree, move, result.next);
        position = result.next;
    }

    return { tree: currentTree, success: true };
}

/**
 * ツリーのクローンを作成（ディープコピー）
 * ノードオブジェクトも複製し、元ツリーとの参照共有を防ぐ
 */
function cloneKifuTree(tree: KifuTree): KifuTree {
    const newNodes = new Map<string, KifuNode>();
    for (const [id, node] of tree.nodes) {
        newNodes.set(id, { ...node, children: [...node.children] });
    }
    return {
        ...tree,
        nodes: newNodes,
    };
}
