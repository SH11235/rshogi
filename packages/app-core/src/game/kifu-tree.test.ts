import { describe, expect, it } from "vitest";
import { createEmptyHands, createInitialBoard, type PositionState } from "./board";
import {
    addMove,
    createKifuTree,
    getBranchInfo,
    getCurrentNode,
    getMainLineMoves,
    getMainLineTotalPly,
    getMovesToCurrent,
    goBack,
    goForward,
    goToEnd,
    goToPly,
    goToStart,
    hasBranchAtCurrent,
    isRewound,
    promoteToMainLine,
    setNodeComment,
    setNodeEval,
    switchBranch,
    truncateFromCurrent,
} from "./kifu-tree";

function createTestPosition(ply: number = 0): PositionState {
    return {
        board: createInitialBoard(),
        hands: createEmptyHands(),
        turn: ply % 2 === 0 ? "sente" : "gote",
        ply,
    };
}

describe("kifu-tree", () => {
    describe("createKifuTree", () => {
        it("開始局面でツリーを作成できる", () => {
            const startPosition = createTestPosition(0);
            const tree = createKifuTree(startPosition, "startpos");

            expect(tree.rootId).toBeDefined();
            expect(tree.currentNodeId).toBe(tree.rootId);
            expect(tree.startSfen).toBe("startpos");

            const rootNode = getCurrentNode(tree);
            expect(rootNode.ply).toBe(0);
            expect(rootNode.usiMove).toBeNull();
            expect(rootNode.parentId).toBeNull();
            expect(rootNode.children).toHaveLength(0);
        });
    });

    describe("addMove", () => {
        it("手を追加できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");

            const positionAfter = createTestPosition(1);
            tree = addMove(tree, "7g7f", positionAfter);

            const currentNode = getCurrentNode(tree);
            expect(currentNode.usiMove).toBe("7g7f");
            expect(currentNode.ply).toBe(1);
        });

        it("複数の手を連続して追加できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");

            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            tree = addMove(tree, "2g2f", createTestPosition(3));

            const currentNode = getCurrentNode(tree);
            expect(currentNode.ply).toBe(3);
            expect(getMovesToCurrent(tree)).toEqual(["7g7f", "3c3d", "2g2f"]);
        });

        it("同じ手を追加した場合は既存ノードに移動する", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");

            tree = addMove(tree, "7g7f", createTestPosition(1));
            const nodeIdAfterFirst = tree.currentNodeId;

            tree = goBack(tree);
            tree = addMove(tree, "7g7f", createTestPosition(1));

            expect(tree.currentNodeId).toBe(nodeIdAfterFirst);
            expect(tree.nodes.size).toBe(2); // ルート + 1手目
        });

        it("別の手を追加した場合は分岐が作成される", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");

            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = goBack(tree);
            tree = addMove(tree, "2g2f", createTestPosition(1));

            expect(tree.nodes.size).toBe(3); // ルート + 2つの分岐

            const rootNode = tree.nodes.get(tree.rootId)!;
            expect(rootNode.children).toHaveLength(2);
        });
    });

    describe("navigation", () => {
        it("goBack: 1手戻れる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));

            tree = goBack(tree);
            expect(getCurrentNode(tree).ply).toBe(1);

            tree = goBack(tree);
            expect(getCurrentNode(tree).ply).toBe(0);
        });

        it("goBack: ルートではそれ以上戻れない", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");

            tree = goBack(tree);
            expect(tree.currentNodeId).toBe(tree.rootId);
        });

        it("goForward: 1手進める", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            tree = goToStart(tree);

            tree = goForward(tree);
            expect(getCurrentNode(tree).ply).toBe(1);

            tree = goForward(tree);
            expect(getCurrentNode(tree).ply).toBe(2);
        });

        it("goForward: 末端ではそれ以上進めない", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            const nodeIdBefore = tree.currentNodeId;
            tree = goForward(tree);
            expect(tree.currentNodeId).toBe(nodeIdBefore);
        });

        it("goToStart: 最初に戻れる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));

            tree = goToStart(tree);
            expect(tree.currentNodeId).toBe(tree.rootId);
            expect(getCurrentNode(tree).ply).toBe(0);
        });

        it("goToEnd: 最後まで進める", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            tree = addMove(tree, "2g2f", createTestPosition(3));
            tree = goToStart(tree);

            tree = goToEnd(tree);
            expect(getCurrentNode(tree).ply).toBe(3);
        });

        it("goToPly: 指定手数に移動できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            tree = addMove(tree, "2g2f", createTestPosition(3));

            tree = goToPly(tree, 1);
            expect(getCurrentNode(tree).ply).toBe(1);

            tree = goToPly(tree, 0);
            expect(getCurrentNode(tree).ply).toBe(0);

            tree = goToPly(tree, 3);
            expect(getCurrentNode(tree).ply).toBe(3);
        });

        it("goToPly: 存在しない手数を指定した場合は最も近い位置に移動", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));

            tree = goToPly(tree, 100);
            expect(getCurrentNode(tree).ply).toBe(2);
        });
    });

    describe("branch management", () => {
        it("switchBranch: 分岐を切り替えられる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = goBack(tree);
            tree = addMove(tree, "2g2f", createTestPosition(1));

            // 2g2fにいる状態で7g7fに切り替え
            tree = switchBranch(tree, 0);
            expect(getCurrentNode(tree).usiMove).toBe("7g7f");

            // 7g7fにいる状態で2g2fに切り替え
            tree = switchBranch(tree, 1);
            expect(getCurrentNode(tree).usiMove).toBe("2g2f");
        });

        it("hasBranchAtCurrent: 分岐の有無を判定できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            expect(hasBranchAtCurrent(tree)).toBe(false);

            tree = goBack(tree);
            tree = addMove(tree, "2g2f", createTestPosition(1));

            expect(hasBranchAtCurrent(tree)).toBe(true);
        });

        it("getBranchInfo: 分岐情報を取得できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = goBack(tree);
            tree = addMove(tree, "2g2f", createTestPosition(1));
            tree = goBack(tree);
            tree = addMove(tree, "5g5f", createTestPosition(1));

            const info = getBranchInfo(tree);
            expect(info.hasBranches).toBe(true);
            expect(info.count).toBe(3);
            expect(info.currentIndex).toBe(2); // 5g5fは3番目
        });

        it("promoteToMainLine: 現在の分岐をメインラインに昇格できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = goBack(tree);
            tree = addMove(tree, "2g2f", createTestPosition(1));

            // 2g2fをメインに昇格
            tree = promoteToMainLine(tree);

            const rootNode = tree.nodes.get(tree.rootId)!;
            const firstChild = tree.nodes.get(rootNode.children[0])!;
            expect(firstChild.usiMove).toBe("2g2f");
        });
    });

    describe("getMovesToCurrent / getMainLineMoves", () => {
        it("getMovesToCurrent: 現在位置までの手を取得できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            tree = addMove(tree, "2g2f", createTestPosition(3));

            expect(getMovesToCurrent(tree)).toEqual(["7g7f", "3c3d", "2g2f"]);

            tree = goBack(tree);
            expect(getMovesToCurrent(tree)).toEqual(["7g7f", "3c3d"]);

            tree = goToStart(tree);
            expect(getMovesToCurrent(tree)).toEqual([]);
        });

        it("getMainLineMoves: メインラインの手を取得できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            tree = goBack(tree);
            tree = addMove(tree, "8c8d", createTestPosition(2)); // 分岐

            // メインラインは7g7f -> 3c3d
            expect(getMainLineMoves(tree)).toEqual(["7g7f", "3c3d"]);
        });
    });

    describe("truncateFromCurrent", () => {
        it("現在位置以降の手を削除できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            tree = addMove(tree, "2g2f", createTestPosition(3));
            tree = goToPly(tree, 1);

            tree = truncateFromCurrent(tree);

            expect(getMainLineMoves(tree)).toEqual(["7g7f"]);
            expect(tree.nodes.size).toBe(2); // ルート + 1手目
        });
    });

    describe("isRewound / getMainLineTotalPly", () => {
        it("isRewound: 巻き戻し状態を判定できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));

            expect(isRewound(tree)).toBe(false);

            tree = goBack(tree);
            expect(isRewound(tree)).toBe(true);

            tree = goToStart(tree);
            expect(isRewound(tree)).toBe(true);
        });

        it("getMainLineTotalPly: メインラインの総手数を取得できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            tree = addMove(tree, "2g2f", createTestPosition(3));

            expect(getMainLineTotalPly(tree)).toBe(3);

            tree = goBack(tree);
            expect(getMainLineTotalPly(tree)).toBe(3); // 巻き戻しても総手数は変わらない
        });
    });

    describe("setNodeEval / setNodeComment", () => {
        it("ノードに評価値を設定できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeEval(tree, tree.currentNodeId, { scoreCp: 100, depth: 20 });

            const node = getCurrentNode(tree);
            expect(node.eval?.scoreCp).toBe(100);
            expect(node.eval?.depth).toBe(20);
        });

        it("ノードにコメントを設定できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeComment(tree, tree.currentNodeId, "良い手");

            const node = getCurrentNode(tree);
            expect(node.comment).toBe("良い手");
        });
    });
});
