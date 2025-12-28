/**
 * 分岐ツリー表示用ユーティリティ
 *
 * KifuTreeからツリービュー表示用のデータ構造を生成する
 */

import {
    BOARD_FILES,
    BOARD_RANKS,
    type KifuNode,
    type KifuTree,
    type Player,
    type Square,
} from "@shogi/app-core";
import { formatMoveSimple } from "./kifFormat";

/**
 * 文字列がSquare型として有効かどうかを判定するtype guard
 */
function isSquare(value: string): value is Square {
    if (value.length !== 2) return false;
    const file = value[0];
    const rank = value[1];
    return (
        (BOARD_FILES as readonly string[]).includes(file) &&
        (BOARD_RANKS as readonly string[]).includes(rank)
    );
}

/** ツリービュー用のノードデータ */
export interface BranchTreeNode {
    /** ノードID */
    nodeId: string;
    /** 手数 */
    ply: number;
    /** 表示テキスト（例: "☗7六歩"） */
    displayText: string;
    /** USI形式の指し手 */
    usiMove: string | null;
    /** 評価値（センチポーン） */
    evalCp?: number;
    /** 詰み手数 */
    evalMate?: number;
    /** 分岐があるか */
    hasBranches: boolean;
    /** 分岐数 */
    branchCount: number;
    /** メインラインか */
    isMainLine: boolean;
    /** 現在のパス上か */
    isCurrentPath: boolean;
    /** 現在位置か */
    isCurrent: boolean;
    /** 子ノード */
    children: BranchTreeNode[];
}

/** 分岐情報（インライン表示用） */
export interface BranchOption {
    /** ノードID */
    nodeId: string;
    /** 表示テキスト */
    displayText: string;
    /** USI形式の指し手 */
    usiMove: string;
    /** 評価値 */
    evalCp?: number;
    /** 詰み手数 */
    evalMate?: number;
    /** メインラインか */
    isMainLine: boolean;
    /** 現在選択中か */
    isSelected: boolean;
    /** この分岐のchildren[index] */
    branchIndex: number;
}

/**
 * 現在位置からルートまでのパスを取得
 */
function getPathToRoot(tree: KifuTree): Set<string> {
    const path = new Set<string>();
    let nodeId: string | null = tree.currentNodeId;

    while (nodeId !== null) {
        path.add(nodeId);
        const node = tree.nodes.get(nodeId);
        if (!node) break;
        nodeId = node.parentId;
    }

    return path;
}

/**
 * ノードの手番を取得
 */
function getNodeTurn(ply: number): Player {
    // ply 0 = ルート（開始局面）
    // ply 1 = 先手の1手目
    // ply 2 = 後手の1手目
    return ply % 2 === 1 ? "sente" : "gote";
}

/**
 * USI形式の指し手から移動先マスを取得
 */
function getToSquare(usiMove: string | null): Square | undefined {
    if (!usiMove || usiMove.length < 4) return undefined;

    // 駒打ち: "P*5e" または 通常移動: "7g7f" or "7g7f+"
    const toSquareStr = usiMove[1] === "*" ? usiMove.slice(2, 4) : usiMove.slice(2, 4);

    if (isSquare(toSquareStr)) {
        return toSquareStr;
    }
    return undefined;
}

/**
 * KifuNodeから表示テキストを生成
 */
function getDisplayText(node: KifuNode, prevToSquare: Square | undefined): string {
    if (node.usiMove === null) {
        return "開始局面";
    }

    const turn = getNodeTurn(node.ply);
    return formatMoveSimple(node.usiMove, turn, node.boardBefore, prevToSquare);
}

/**
 * KifuTreeからツリービュー用のデータを構築
 *
 * @param tree 棋譜ツリー
 * @param maxDepth 最大深さ（省略時は全て）
 * @returns ルートノードから始まるツリーデータ
 */
export function buildBranchTreeData(tree: KifuTree, maxDepth?: number): BranchTreeNode {
    const currentPath = getPathToRoot(tree);

    function buildNode(
        nodeId: string,
        isMainLine: boolean,
        depth: number,
        prevToSquare: Square | undefined,
    ): BranchTreeNode | null {
        const node = tree.nodes.get(nodeId);
        if (!node) return null;

        // 最大深さチェック
        if (maxDepth !== undefined && depth > maxDepth) {
            return null;
        }

        const displayText = getDisplayText(node, prevToSquare);
        const toSquare = getToSquare(node.usiMove);

        // 子ノードを構築
        const children: BranchTreeNode[] = [];
        for (let i = 0; i < node.children.length; i++) {
            const childId = node.children[i];
            const isChildMainLine = isMainLine && i === 0;
            const childNode = buildNode(childId, isChildMainLine, depth + 1, toSquare);
            if (childNode) {
                children.push(childNode);
            }
        }

        return {
            nodeId,
            ply: node.ply,
            displayText,
            usiMove: node.usiMove,
            evalCp: node.eval?.scoreCp,
            evalMate: node.eval?.scoreMate,
            hasBranches: node.children.length > 1,
            branchCount: node.children.length,
            isMainLine,
            isCurrentPath: currentPath.has(nodeId),
            isCurrent: nodeId === tree.currentNodeId,
            children,
        };
    }

    const rootNode = buildNode(tree.rootId, true, 0, undefined);
    if (!rootNode) {
        throw new Error("Failed to build tree data: root node not found");
    }

    return rootNode;
}

/**
 * 指定ノードの分岐オプションを取得
 *
 * @param tree 棋譜ツリー
 * @param nodeId 分岐があるノードのID
 * @returns 分岐オプションの配列
 */
export function getBranchOptions(tree: KifuTree, nodeId: string): BranchOption[] {
    const node = tree.nodes.get(nodeId);
    if (!node || node.children.length <= 1) {
        return [];
    }

    const currentPath = getPathToRoot(tree);
    const toSquare = getToSquare(node.usiMove);

    const result: BranchOption[] = [];

    for (let index = 0; index < node.children.length; index++) {
        const childId = node.children[index];
        const childNode = tree.nodes.get(childId);
        if (!childNode) continue;

        const displayText = getDisplayText(childNode, toSquare);

        const option: BranchOption = {
            nodeId: childId,
            displayText,
            usiMove: childNode.usiMove ?? "",
            isMainLine: index === 0,
            isSelected: currentPath.has(childId),
            branchIndex: index,
        };

        if (childNode.eval?.scoreCp !== undefined) {
            option.evalCp = childNode.eval.scoreCp;
        }
        if (childNode.eval?.scoreMate !== undefined) {
            option.evalMate = childNode.eval.scoreMate;
        }

        result.push(option);
    }

    return result;
}

/**
 * ツリーをフラットなリストに変換（メインライン優先）
 * 分岐情報を保持しつつ、表示用にフラット化する
 */
export interface FlatTreeNode {
    /** ノードID */
    nodeId: string;
    /** 手数 */
    ply: number;
    /** 表示テキスト */
    displayText: string;
    /** USI形式の指し手 */
    usiMove: string | null;
    /** 評価値 */
    evalCp?: number;
    /** 詰み手数 */
    evalMate?: number;
    /** 分岐があるか */
    hasBranches: boolean;
    /** 分岐オプション（分岐がある場合のみ） */
    branchOptions?: BranchOption[];
    /** 現在のパス上か */
    isCurrentPath: boolean;
    /** 現在位置か */
    isCurrent: boolean;
    /** ネスト深さ（分岐の深さ） */
    nestLevel: number;
}

/**
 * 現在のパスに沿ってツリーをフラット化
 *
 * @param tree 棋譜ツリー
 * @returns フラット化されたノードリスト
 */
export function flattenTreeAlongCurrentPath(tree: KifuTree): FlatTreeNode[] {
    const result: FlatTreeNode[] = [];
    const currentPath = getPathToRoot(tree);

    // ルートから現在位置までのパスを取得
    const pathFromRoot: string[] = [];
    let nodeId: string | null = tree.currentNodeId;
    while (nodeId !== null) {
        pathFromRoot.unshift(nodeId);
        const node = tree.nodes.get(nodeId);
        if (!node) break;
        nodeId = node.parentId;
    }

    // 現在位置から先のメインラインも取得
    let currentNode = tree.nodes.get(tree.currentNodeId);
    while (currentNode && currentNode.children.length > 0) {
        const firstChildId = currentNode.children[0];
        pathFromRoot.push(firstChildId);
        currentNode = tree.nodes.get(firstChildId);
    }

    // パスに沿ってフラット化
    let prevToSquare: Square | undefined;
    for (const nid of pathFromRoot) {
        const node = tree.nodes.get(nid);
        if (!node) continue;

        const displayText = getDisplayText(node, prevToSquare);
        const hasBranches = node.children.length > 1;

        const flatNode: FlatTreeNode = {
            nodeId: nid,
            ply: node.ply,
            displayText,
            usiMove: node.usiMove,
            evalCp: node.eval?.scoreCp,
            evalMate: node.eval?.scoreMate,
            hasBranches,
            isCurrentPath: currentPath.has(nid),
            isCurrent: nid === tree.currentNodeId,
            nestLevel: 0,
        };

        // 分岐がある場合はオプションを追加
        if (hasBranches) {
            flatNode.branchOptions = getBranchOptions(tree, nid);
        }

        result.push(flatNode);
        prevToSquare = getToSquare(node.usiMove);
    }

    return result;
}

/**
 * ツリー内の全分岐点を取得
 *
 * @param tree 棋譜ツリー
 * @returns 分岐点のノードIDと手数のマップ
 */
export function getAllBranchPoints(tree: KifuTree): Map<number, string[]> {
    const branchPoints = new Map<number, string[]>();

    for (const [nodeId, node] of tree.nodes) {
        if (node.children.length > 1) {
            const existing = branchPoints.get(node.ply) ?? [];
            existing.push(nodeId);
            branchPoints.set(node.ply, existing);
        }
    }

    return branchPoints;
}

/** 分岐情報（一覧表示用） */
export interface BranchSummary {
    /** 分岐点のノードID */
    parentNodeId: string;
    /** 分岐点の手数 */
    ply: number;
    /** 分岐の子ノードID */
    nodeId: string;
    /** 分岐インデックス（0=メインライン） */
    branchIndex: number;
    /** 表示テキスト（例: "☗7六歩"） */
    displayText: string;
    /** 分岐後の手数 */
    branchLength: number;
    /** メインラインか */
    isMainLine: boolean;
    /** タブ表示用のラベル（例: "12手目△3四歩の変化"） */
    tabLabel: string;
}

/**
 * 分岐ラインの手数を取得（メインラインに沿って数える）
 */
function countBranchLength(tree: KifuTree, startNodeId: string): number {
    let count = 0;
    let nodeId: string | null = startNodeId;

    while (nodeId) {
        count++;
        const node = tree.nodes.get(nodeId);
        if (!node || node.children.length === 0) break;
        nodeId = node.children[0]; // メインラインを辿る
    }

    return count;
}

/**
 * ツリー内の全分岐を取得（一覧表示用）
 * メインラインからの分岐のみを返す（ネストした分岐は除外）
 *
 * @param tree 棋譜ツリー
 * @returns 分岐情報の配列（手数順）
 */
export function getAllBranches(tree: KifuTree): BranchSummary[] {
    const branches: BranchSummary[] = [];

    // メインラインを辿りながら分岐を収集
    let nodeId: string | null = tree.rootId;

    while (nodeId) {
        const node = tree.nodes.get(nodeId);
        if (!node) break;

        // このノードに分岐がある場合
        if (node.children.length > 1) {
            const toSquare = getToSquare(node.usiMove);

            // メインライン以外の子ノード（分岐）を追加
            for (let i = 1; i < node.children.length; i++) {
                const childId = node.children[i];
                const childNode = tree.nodes.get(childId);
                if (!childNode) continue;

                const displayText = getDisplayText(childNode, toSquare);
                const branchLength = countBranchLength(tree, childId);

                branches.push({
                    parentNodeId: nodeId,
                    ply: node.ply,
                    nodeId: childId,
                    branchIndex: i,
                    displayText,
                    branchLength,
                    isMainLine: false,
                    tabLabel: `${childNode.ply}手目の変化`,
                });
            }
        }

        // メインライン（最初の子）を辿る
        nodeId = node.children.length > 0 ? node.children[0] : null;
    }

    return branches;
}

/**
 * 指定した分岐の手順をリストとして取得
 * 分岐点以前の本譜も含めて返す
 *
 * @param tree 棋譜ツリー
 * @param branchNodeId 分岐の開始ノードID
 * @returns 分岐の手順リスト（本譜 + 分岐）
 */
export function getBranchMoves(tree: KifuTree, branchNodeId: string): FlatTreeNode[] {
    const result: FlatTreeNode[] = [];
    const currentPath = getPathToRoot(tree);

    const branchNode = tree.nodes.get(branchNodeId);
    if (!branchNode) return result;

    // 1. 分岐点の親ノードまでの本譜を取得
    const mainLinePath: string[] = [];
    let nodeId: string | null = tree.rootId;

    // ルートから分岐点の親まで辿る
    while (nodeId && nodeId !== branchNode.parentId) {
        mainLinePath.push(nodeId);
        const node = tree.nodes.get(nodeId);
        if (!node || node.children.length === 0) break;
        // メインライン（最初の子）を辿る
        nodeId = node.children[0];
    }
    // 分岐点の親も追加
    if (branchNode.parentId) {
        mainLinePath.push(branchNode.parentId);
    }

    // 本譜部分をリストに追加（ルートノードは除く）
    let prevToSquare: Square | undefined;
    for (const nid of mainLinePath) {
        const node = tree.nodes.get(nid);
        if (!node) continue;

        // ルートノード（ply 0）は開始局面なので除外
        if (node.ply === 0) {
            prevToSquare = getToSquare(node.usiMove);
            continue;
        }

        const displayText = getDisplayText(node, prevToSquare);
        const hasBranches = node.children.length > 1;

        result.push({
            nodeId: nid,
            ply: node.ply,
            displayText,
            usiMove: node.usiMove,
            evalCp: node.eval?.scoreCp,
            evalMate: node.eval?.scoreMate,
            hasBranches,
            isCurrentPath: currentPath.has(nid),
            isCurrent: nid === tree.currentNodeId,
            nestLevel: 0,
        });

        prevToSquare = getToSquare(node.usiMove);
    }

    // 2. 分岐部分を追加
    nodeId = branchNodeId;
    while (nodeId) {
        const node = tree.nodes.get(nodeId);
        if (!node) break;

        const displayText = getDisplayText(node, prevToSquare);
        const hasBranches = node.children.length > 1;

        result.push({
            nodeId,
            ply: node.ply,
            displayText,
            usiMove: node.usiMove,
            evalCp: node.eval?.scoreCp,
            evalMate: node.eval?.scoreMate,
            hasBranches,
            isCurrentPath: currentPath.has(nodeId),
            isCurrent: nodeId === tree.currentNodeId,
            nestLevel: 1, // 分岐部分はnestLevel=1で区別
        });

        prevToSquare = getToSquare(node.usiMove);
        // メインライン（最初の子）を辿る
        nodeId = node.children.length > 0 ? node.children[0] : null;
    }

    return result;
}
