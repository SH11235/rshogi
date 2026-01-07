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
    setNodeMultiPvEval,
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

            const rootNode = tree.nodes.get(tree.rootId);
            expect(rootNode?.children).toHaveLength(2);
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

        it("goToPly: 進む場合はchildren[0]を辿る（メインラインに戻る）", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            // メインライン: 7g7f -> 3c3d
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            // 分岐作成: 開始位置に戻って 2g2f -> 8c8d を追加
            tree = goToStart(tree);
            tree = addMove(tree, "2g2f", createTestPosition(1));
            tree = addMove(tree, "8c8d", createTestPosition(2));

            // 現在は分岐上の ply=2 (8c8d)
            expect(getCurrentNode(tree).usiMove).toBe("8c8d");

            // ply=0 に戻る
            tree = goToPly(tree, 0);
            expect(getCurrentNode(tree).ply).toBe(0);

            // ply=2 に進む -> children[0]を辿るのでメインライン(7g7f -> 3c3d)に行く
            tree = goToPly(tree, 2);
            expect(getCurrentNode(tree).ply).toBe(2);
            expect(getCurrentNode(tree).usiMove).toBe("3c3d");
        });

        it("goToPly: 分岐の途中から進む場合は現在ラインのchildren[0]を辿る", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            // メインライン: 7g7f -> 3c3d -> 2g2f
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            tree = addMove(tree, "2g2f", createTestPosition(3));
            // 分岐作成: 7g7f に戻って 8c8d -> 6g6f を追加
            tree = goToPly(tree, 1);
            tree = addMove(tree, "8c8d", createTestPosition(2));
            tree = addMove(tree, "6g6f", createTestPosition(3));

            // 現在は分岐上の ply=3 (6g6f)
            expect(getCurrentNode(tree).usiMove).toBe("6g6f");

            // ply=1 に戻る
            tree = goToPly(tree, 1);
            expect(getCurrentNode(tree).ply).toBe(1);
            expect(getCurrentNode(tree).usiMove).toBe("7g7f");

            // ply=3 に進む -> children[0]を辿るのでメインライン(3c3d -> 2g2f)に行く
            tree = goToPly(tree, 3);
            expect(getCurrentNode(tree).ply).toBe(3);
            expect(getCurrentNode(tree).usiMove).toBe("2g2f");
        });

        it("goToPly: 分岐上で戻る場合、現在ラインの親を辿る", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            // メインライン: 7g7f -> 3c3d -> 2g2f
            tree = addMove(tree, "7g7f", createTestPosition(1));
            tree = addMove(tree, "3c3d", createTestPosition(2));
            tree = addMove(tree, "2g2f", createTestPosition(3));
            // 分岐作成: 7g7f に戻って 8c8d -> 6g6f を追加
            tree = goToPly(tree, 1);
            tree = addMove(tree, "8c8d", createTestPosition(2));
            tree = addMove(tree, "6g6f", createTestPosition(3));

            // 現在は分岐上の ply=3 (6g6f)
            expect(getCurrentNode(tree).usiMove).toBe("6g6f");

            // ply=2 に戻る -> 分岐上の 8c8d に行くべき（メインラインの 3c3d ではない）
            tree = goToPly(tree, 2);
            expect(getCurrentNode(tree).ply).toBe(2);
            expect(getCurrentNode(tree).usiMove).toBe("8c8d");

            // ply=1 に戻る -> 共通の 7g7f
            tree = goToPly(tree, 1);
            expect(getCurrentNode(tree).ply).toBe(1);
            expect(getCurrentNode(tree).usiMove).toBe("7g7f");
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

            const rootNode = tree.nodes.get(tree.rootId);
            const firstChild = rootNode ? tree.nodes.get(rootNode.children[0]) : undefined;
            expect(firstChild?.usiMove).toBe("2g2f");
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

    describe("setNodeMultiPvEval", () => {
        it("multipv=1 の評価値を設定できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 20,
                pv: ["3c3d"],
            });

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals).toHaveLength(1);
            expect(node.multiPvEvals?.[0]?.scoreCp).toBe(100);
            expect(node.multiPvEvals?.[0]?.depth).toBe(20);
            expect(node.multiPvEvals?.[0]?.pv).toEqual(["3c3d"]);
        });

        it("multipv=2 の評価値を設定できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 20,
            });
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 2, {
                scoreCp: 50,
                depth: 20,
            });

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals).toHaveLength(2);
            expect(node.multiPvEvals?.[0]?.scoreCp).toBe(100);
            expect(node.multiPvEvals?.[1]?.scoreCp).toBe(50);
        });

        it("multipv=3 の評価値を設定できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 20,
            });
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 2, {
                scoreCp: 50,
                depth: 20,
            });
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 3, {
                scoreCp: 0,
                depth: 20,
            });

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals).toHaveLength(3);
            expect(node.multiPvEvals?.[2]?.scoreCp).toBe(0);
        });

        it("同じmultipvの評価値を上書きできる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 15,
            });
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 150,
                depth: 20,
            });

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals).toHaveLength(1);
            expect(node.multiPvEvals?.[0]?.scoreCp).toBe(150);
            expect(node.multiPvEvals?.[0]?.depth).toBe(20);
        });

        it("深さが深い評価値で上書きされる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 20,
            });
            // 深さが浅い場合は上書きされない
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 50,
                depth: 15,
            });

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals?.[0]?.scoreCp).toBe(100);
            expect(node.multiPvEvals?.[0]?.depth).toBe(20);
        });

        it("multipv=2 を先に設定し、後から multipv=1 を設定しても順序が正しい", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 2, {
                scoreCp: 50,
                depth: 20,
            });
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 20,
            });

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals).toHaveLength(2);
            expect(node.multiPvEvals?.[0]?.scoreCp).toBe(100);
            expect(node.multiPvEvals?.[1]?.scoreCp).toBe(50);
        });

        it("存在しないノードIDの場合はツリーを変更しない", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            const treeBefore = tree;
            tree = setNodeMultiPvEval(tree, "non-existent-id", 1, {
                scoreCp: 100,
                depth: 20,
            });

            expect(tree).toBe(treeBefore);
        });

        it("空のevalDataでも設定できる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {});

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals).toHaveLength(1);
            expect(node.multiPvEvals?.[0]).toEqual({});
        });

        it("スパース配列を正しく扱える（multipv=1と3を設定、2は未定義）", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 20,
            });
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 3, {
                scoreCp: 50,
                depth: 20,
            });

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals).toHaveLength(3);
            expect(node.multiPvEvals?.[0]).toBeDefined();
            expect(node.multiPvEvals?.[1]).toBeUndefined();
            expect(node.multiPvEvals?.[2]).toBeDefined();
            expect(node.multiPvEvals?.[0]?.scoreCp).toBe(100);
            expect(node.multiPvEvals?.[2]?.scoreCp).toBe(50);
        });

        it("深さが同じ場合は上書きされない（PVが既にある場合）", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 20,
                pv: ["3c3d"],
            });
            // 同じ深さで別の評価値を設定しようとしても上書きされない（既存にPVがある場合）
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 80,
                depth: 20,
                pv: ["8c8d"],
            });

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals?.[0]?.scoreCp).toBe(100);
            expect(node.multiPvEvals?.[0]?.pv).toEqual(["3c3d"]);
        });

        it("深さが同じでも既存のPVが空で新しいPVがある場合は上書きされる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            // 最初はPVなしで設定
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 20,
            });
            // 同じ深さでPVありのデータを設定すると上書きされる
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 20,
                pv: ["3c3d"],
            });

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals?.[0]?.pv).toEqual(["3c3d"]);
        });

        it("深さが深い評価値で上書きされる", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 100,
                depth: 20,
            });
            // 深さが深い場合は上書きされる
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1, {
                scoreCp: 150,
                depth: 25,
            });

            const node = getCurrentNode(tree);
            expect(node.multiPvEvals?.[0]?.scoreCp).toBe(150);
            expect(node.multiPvEvals?.[0]?.depth).toBe(25);
        });

        it("multipv < 1 の場合はツリーを変更しない", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            const treeBefore = tree;
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 0, {
                scoreCp: 100,
                depth: 20,
            });

            expect(tree).toBe(treeBefore);
        });

        it("multipvが小数の場合はツリーを変更しない", () => {
            const startPosition = createTestPosition(0);
            let tree = createKifuTree(startPosition, "startpos");
            tree = addMove(tree, "7g7f", createTestPosition(1));

            const treeBefore = tree;
            tree = setNodeMultiPvEval(tree, tree.currentNodeId, 1.5, {
                scoreCp: 100,
                depth: 20,
            });

            expect(tree).toBe(treeBefore);
        });
    });
});
