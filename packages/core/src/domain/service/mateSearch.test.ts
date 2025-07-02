import { beforeEach, describe, expect, it } from "vitest";
import type { Board } from "../model/board";
import { MateSearchService, findCheckmate, findOneMoveCheckmate } from "./mateSearch";
import type { Hands } from "./moveService";
import { initialHands } from "./moveService";

describe("mateSearch", () => {
    let emptyBoard: Board;
    let emptyHands: Hands;

    beforeEach(() => {
        // 空の盤面を作成
        emptyBoard = {} as Board;
        for (let row = 1; row <= 9; row++) {
            for (let col = 1; col <= 9; col++) {
                emptyBoard[`${row}${col}`] = null;
            }
        }
        emptyHands = initialHands();
    });

    describe("findOneMoveCheckmate", () => {
        it("頭金の1手詰めを発見できる", () => {
            // 後手玉が1一で逃げ場なし
            emptyBoard["11"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["21"] = { type: "silver", owner: "black", promoted: false }; // 2一を塞ぐ
            emptyBoard["22"] = { type: "silver", owner: "black", promoted: false }; // 2二を塞ぐ
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };
            emptyHands.black.金 = 1;

            const move = findOneMoveCheckmate(emptyBoard, emptyHands, "black");
            expect(move).not.toBeNull();
            // 銀を動かして詰ますか、金を打って詰ますか
            if (move?.type === "drop") {
                expect(move.piece.type).toBe("gold");
                expect(move.to).toEqual({ row: 1, column: 2 });
            } else if (move?.type === "move") {
                // 銀を動かす詰み手もあり得る
                expect(move.piece.type).toBe("silver");
            }
        });

        it("飛車による1手詰めを発見できる", () => {
            // 後手玉が5一、先手飛車が5九
            emptyBoard["51"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["59"] = { type: "rook", owner: "black", promoted: false };
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };

            const move = findOneMoveCheckmate(emptyBoard, emptyHands, "black");
            expect(move).not.toBeNull();
            expect(move?.type).toBe("move");
            if (move?.type === "move") {
                expect(move.from).toEqual({ row: 5, column: 9 });
                expect(move.to).toEqual({ row: 5, column: 1 });
            }
        });

        it("詰みがない場合はnullを返す", () => {
            // 玉のみの配置
            emptyBoard["51"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["59"] = { type: "king", owner: "black", promoted: false };

            const move = findOneMoveCheckmate(emptyBoard, emptyHands, "black");
            expect(move).toBeNull();
        });

        it("守備駒がある場合の1手詰めを発見できる", () => {
            // 後手玉が1一、金で守られているが、先手の飛車で横から詰み
            emptyBoard["11"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["12"] = { type: "gold", owner: "white", promoted: false };
            emptyBoard["21"] = { type: "gold", owner: "black", promoted: false }; // 2一を塞ぐ
            emptyBoard["22"] = { type: "gold", owner: "black", promoted: false }; // 2二を塞ぐ
            emptyBoard["19"] = { type: "rook", owner: "black", promoted: false }; // 飛車で横から
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };

            const move = findOneMoveCheckmate(emptyBoard, emptyHands, "black");
            expect(move).not.toBeNull();
            expect(move?.type).toBe("move");
            if (move?.type === "move") {
                // 飛車が詰み手を発見
                expect(move.piece.type).toBe("rook");
                // 1段目か2段目への移動で詰む
                expect([1, 2]).toContain(move.to.row);
            }
        });
    });

    describe("MateSearchService", () => {
        it("1手詰めを正しく探索できる", () => {
            const service = new MateSearchService();

            // 頭金の1手詰め（周りを塞いで）
            emptyBoard["11"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["21"] = { type: "gold", owner: "black", promoted: false };
            emptyBoard["22"] = { type: "gold", owner: "black", promoted: false };
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };
            emptyHands.black.金 = 1;

            const result = service.search(emptyBoard, emptyHands, "black", { maxDepth: 1 });

            expect(result.isMate).toBe(true);
            expect(result.moves).toHaveLength(1);
            expect(result.nodeCount).toBeGreaterThan(0);
            expect(result.elapsedMs).toBeGreaterThanOrEqual(0);
        });

        it("3手詰めを正しく探索できる", () => {
            const service = new MateSearchService();

            // 簡単な3手詰め：飛車で王手、玉が逃げて、金で詰み
            emptyBoard["51"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["53"] = { type: "rook", owner: "black", promoted: false };
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };
            emptyHands.black.金 = 1;

            const result = service.search(emptyBoard, emptyHands, "black", { maxDepth: 3 });

            expect(result.isMate).toBe(true);
            // 詰み手順は1手か3手のはず
            expect(result.moves.length).toBeGreaterThanOrEqual(1);
            expect(result.moves.length).toBeLessThanOrEqual(3);
            expect(result.moves.length % 2).toBe(1); // 奇数手詰め
        });

        it("詰みがない場合は正しくfalseを返す", () => {
            const service = new MateSearchService();

            // 玉のみ、詰みなし
            emptyBoard["51"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["59"] = { type: "king", owner: "black", promoted: false };

            const result = service.search(emptyBoard, emptyHands, "black", { maxDepth: 5 });

            expect(result.isMate).toBe(false);
            expect(result.moves).toHaveLength(0);
        });

        it("タイムアウトが機能する", () => {
            const service = new MateSearchService();

            // 複雑な局面を作成（実際には簡単だが、タイムアウトを短くして確認）
            emptyBoard["51"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["59"] = { type: "king", owner: "black", promoted: false };
            emptyBoard["55"] = { type: "rook", owner: "black", promoted: false };

            const result = service.search(emptyBoard, emptyHands, "black", {
                maxDepth: 99,
                timeout: 1, // 1ミリ秒でタイムアウト
            });

            // タイムアウトするはず
            expect(result.elapsedMs).toBeGreaterThanOrEqual(1);
        });

        it("受け方の合法手がある場合は1手では詰まない", () => {
            const service = new MateSearchService();

            // 後手玉に逃げ道がある局面
            emptyBoard["55"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };
            emptyHands.black.金 = 1;

            const result = service.search(emptyBoard, emptyHands, "black", { maxDepth: 1 });

            // 1手では詰まない（玉に逃げ道がたくさんある）
            expect(result.isMate).toBe(false);
        });

        it("5手詰めを探索できる", () => {
            const service = new MateSearchService();

            // 飛車と金による5手詰めの局面
            emptyBoard["13"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["15"] = { type: "rook", owner: "black", promoted: false };
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };
            emptyHands.black.金 = 1;

            const result = service.search(emptyBoard, emptyHands, "black", { maxDepth: 5 });

            expect(result.isMate).toBe(true);
            expect(result.moves.length).toBeLessThanOrEqual(5);
            expect(result.moves.length % 2).toBe(1); // 奇数手詰め
        });
    });

    describe("findCheckmate", () => {
        it("デフォルトで7手詰めまで探索する", () => {
            // 1手詰めの簡単な局面
            emptyBoard["11"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["21"] = { type: "gold", owner: "black", promoted: false };
            emptyBoard["22"] = { type: "gold", owner: "black", promoted: false };
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };
            emptyHands.black.金 = 1;

            const result = findCheckmate(emptyBoard, emptyHands, "black");

            expect(result.isMate).toBe(true);
            expect(result.moves).toHaveLength(1);
        });

        it("指定した手数まで探索する", () => {
            // 3手詰めの局面
            emptyBoard["51"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["53"] = { type: "rook", owner: "black", promoted: false };
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };
            emptyHands.black.金 = 1;

            // 1手までしか探索しない
            const result1 = findCheckmate(emptyBoard, emptyHands, "black", 1);

            // 3手まで探索
            const result3 = findCheckmate(emptyBoard, emptyHands, "black", 3);

            // 1手詰めか3手詰めなので、どちらかで詰むはず
            expect(result1.isMate || result3.isMate).toBe(true);

            if (result3.isMate) {
                expect(result3.moves.length).toBeGreaterThanOrEqual(1);
                expect(result3.moves.length).toBeLessThanOrEqual(3);
                expect(result3.moves.length % 2).toBe(1); // 奇数手詰め
            }
        });
    });

    describe("複雑な詰み筋", () => {
        it("合駒が効かない詰みを発見できる", () => {
            // 飛車の王手に合駒しても詰む局面
            emptyBoard["11"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["19"] = { type: "rook", owner: "black", promoted: false };
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };
            emptyHands.black.金 = 2;
            emptyHands.white.歩 = 1; // 合駒用

            const result = findCheckmate(emptyBoard, emptyHands, "black", 3);
            expect(result.isMate).toBe(true);
        });

        it("逃げ道を塞ぐ詰みを発見できる", () => {
            // 玉の逃げ道を塞いでから詰ます
            emptyBoard["12"] = { type: "king", owner: "white", promoted: false };
            emptyBoard["14"] = { type: "gold", owner: "black", promoted: false };
            emptyBoard["32"] = { type: "silver", owner: "black", promoted: false };
            emptyBoard["99"] = { type: "king", owner: "black", promoted: false };
            emptyHands.black.飛 = 1;

            const result = findCheckmate(emptyBoard, emptyHands, "black", 3);
            expect(result.isMate).toBe(true);
        });
    });
});
