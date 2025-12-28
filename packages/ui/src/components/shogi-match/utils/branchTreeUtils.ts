/**
 * 分岐ツリー表示用ユーティリティ
 *
 * KifuTreeからツリービュー表示用のデータ構造を生成する
 */

import {
    BOARD_FILES,
    BOARD_RANKS,
    findNodeByPlyInMainLine,
    type KifuNode,
    type KifuTree,
    type Player,
    type Square,
} from "@shogi/app-core";
import { formatMoveSimple } from "./kifFormat";

/**
 * PVと本譜の比較結果
 */
export interface PvMainLineComparison {
    /** 比較タイプ */
    type:
        | "identical" // PVが本譜と完全一致
        | "diverges_later" // 途中から分岐（1手目は同じ）
        | "diverges_first"; // 最初から異なる
    /** 分岐点の手数（diverges_laterの場合のみ有効） */
    divergePly?: number;
    /** 分岐開始時のPVインデックス（0-based、diverges_laterの場合のみ有効） */
    divergeIndex?: number;
}

/**
 * PVと本譜を比較し、分岐点を検出する
 *
 * @param tree 棋譜ツリー
 * @param basePly PVの起点手数（この手を指した後の局面からPVが始まる）
 * @param pv PV（読み筋）の手順
 * @returns 比較結果
 */
export function comparePvWithMainLine(
    tree: KifuTree,
    basePly: number,
    pv: string[],
): PvMainLineComparison {
    if (pv.length === 0) {
        return { type: "identical" };
    }

    // basePlyが負の値の場合は無効
    if (basePly < 0) {
        return { type: "diverges_first" };
    }

    // basePlyのノードを取得
    const baseNodeId = findNodeByPlyInMainLine(tree, basePly);
    if (!baseNodeId) {
        // ノードが見つからない場合は「最初から異なる」として扱う
        return { type: "diverges_first" };
    }

    const baseNode = tree.nodes.get(baseNodeId);
    if (!baseNode) {
        return { type: "diverges_first" };
    }

    // メインラインを辿りながらPVと比較
    let currentNode = baseNode;

    for (let i = 0; i < pv.length; i++) {
        const pvMove = pv[i];

        // 次のメインラインの手を取得
        if (currentNode.children.length === 0) {
            // メインラインの終端に達した場合
            // 残りのPVは新規分岐となる
            if (i === 0) {
                return { type: "diverges_first" };
            }
            return {
                type: "diverges_later",
                divergePly: currentNode.ply,
                divergeIndex: i,
            };
        }

        const mainLineChildId = currentNode.children[0];
        const mainLineChild = tree.nodes.get(mainLineChildId);
        if (!mainLineChild) {
            return { type: "diverges_first" };
        }

        // PVの手とメインラインの手を比較
        if (mainLineChild.usiMove !== pvMove) {
            // 分岐点発見
            if (i === 0) {
                return { type: "diverges_first" };
            }
            return {
                type: "diverges_later",
                divergePly: currentNode.ply,
                divergeIndex: i,
            };
        }

        // 次のノードへ
        currentNode = mainLineChild;
    }

    // すべてのPVの手がメインラインと一致
    return { type: "identical" };
}

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
    /** 現在のパス上か */
    isCurrentPath: boolean;
    /** 現在位置か */
    isCurrent: boolean;
    /** ネスト深さ（分岐の深さ） */
    nestLevel: number;
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
