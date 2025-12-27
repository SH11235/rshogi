/**
 * 分岐ツリー表示用ユーティリティ
 *
 * KifuTreeからツリービュー表示用のデータ構造を生成する
 */

import type { KifuNode, KifuTree } from "@shogi/app-core";
import type { Player } from "@shogi/app-core";
import { formatMoveSimple } from "./kifFormat";

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
 * KifuNodeから表示テキストを生成
 */
function getDisplayText(node: KifuNode, prevToSquare: string | undefined): string {
    if (node.usiMove === null) {
        return "開始局面";
    }

    const turn = getNodeTurn(node.ply);
    return formatMoveSimple(node.usiMove, turn, node.boardBefore, prevToSquare as any);
}

/**
 * USI形式の指し手から移動先マスを取得
 */
function getToSquare(usiMove: string | null): string | undefined {
    if (!usiMove || usiMove.length < 4) return undefined;

    // 駒打ち: "P*5e"
    if (usiMove[1] === "*") {
        return usiMove.slice(2, 4);
    }

    // 通常移動: "7g7f" or "7g7f+"
    return usiMove.slice(2, 4);
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
        prevToSquare: string | undefined,
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
    let prevToSquare: string | undefined;
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
