import type { Board } from "@/domain/model/board";
import { setPiece } from "@/domain/model/board";
import type { Move } from "@/domain/model/move";
import type { Piece } from "@/domain/model/piece";
import {
    applyMove,
    createEmptyHands,
    generateMoves,
    hasNifuViolation,
    isDeadPiece,
    isUchifuzume,
    revertMove,
    toggleSide,
} from "@/domain/service/moveService";
import { describe, expect, it } from "vitest";
import type { Column, Row, Square } from "../model/square";

//----------------------------------------------------------------
// Helper utilities for tests
//----------------------------------------------------------------
const sq = (row: Row, col: Column): Square => ({ row, column: col });

const makePiece = (kind: Piece["kind"], owner: Piece["owner"], promoted = false): Piece => ({
    kind,
    owner,
    promoted,
});

const nullBoard: Board = {
    11: null,
    12: null,
    13: null,
    14: null,
    15: null,
    16: null,
    17: null,
    18: null,
    19: null,
    21: null,
    22: null,
    23: null,
    24: null,
    25: null,
    26: null,
    27: null,
    28: null,
    29: null,
    31: null,
    32: null,
    33: null,
    34: null,
    35: null,
    36: null,
    37: null,
    38: null,
    39: null,
    41: null,
    42: null,
    43: null,
    44: null,
    45: null,
    46: null,
    47: null,
    48: null,
    49: null,
    51: null,
    52: null,
    53: null,
    54: null,
    55: null,
    56: null,
    57: null,
    58: null,
    59: null,
    61: null,
    62: null,
    63: null,
    64: null,
    65: null,
    66: null,
    67: null,
    68: null,
    69: null,
    71: null,
    72: null,
    73: null,
    74: null,
    75: null,
    76: null,
    77: null,
    78: null,
    79: null,
    81: null,
    82: null,
    83: null,
    84: null,
    85: null,
    86: null,
    87: null,
    88: null,
    89: null,
    91: null,
    92: null,
    93: null,
    94: null,
    95: null,
    96: null,
    97: null,
    98: null,
    99: null,
};

//----------------------------------------------------------------
// Test cases
//----------------------------------------------------------------

describe("toggleSide", () => {
    it("手番を反転する", () => {
        expect(toggleSide("black")).toBe("white");
        expect(toggleSide("white")).toBe("black");
    });
});

describe("applyMove / revertMove", () => {
    it("通常の移動を適用・巻き戻し", () => {
        // 7六に黒の歩
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(7, 6), makePiece("歩", "black"));
        const hands = createEmptyHands();

        const move: Move = {
            type: "move",
            from: sq(7, 6),
            to: sq(6, 6),
            piece: makePiece("歩", "black"),
            promote: false,
            captured: null,
        };

        const applied = applyMove(board, hands, "black", move);
        expect(applied.board["76"]).toBeNull();
        expect(applied.board["66"]).toMatchObject({ kind: "歩", owner: "black", promoted: false });
        expect(applied.nextTurn).toBe("white");

        // 巻き戻し
        const reverted = revertMove(applied.board, applied.hands, applied.nextTurn, move);
        expect(reverted.board["76"]).toMatchObject({ kind: "歩", owner: "black", promoted: false });
        expect(reverted.board["66"]).toBeNull();
        expect(reverted.nextTurn).toBe("black");
    });

    it("成りを適用・巻き戻し", () => {
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(2, 2), makePiece("角", "white"));
        const hands = createEmptyHands();

        const move: Move = {
            type: "move",
            from: sq(2, 2),
            to: sq(8, 8),
            piece: makePiece("角", "white"),
            promote: true,
            captured: null,
        };

        const applied = applyMove(board, hands, "white", move);
        expect(applied.board["88"]).toMatchObject({ kind: "角", owner: "white", promoted: true });

        const reverted = revertMove(applied.board, applied.hands, applied.nextTurn, move);
        expect(reverted.board["22"]).toMatchObject({ kind: "角", owner: "white", promoted: false });
    });

    it("駒の取得を適用・巻き戻し", () => {
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(2, 2), makePiece("角", "white"));
        board = setPiece(board, sq(8, 8), makePiece("飛", "black"));
        const hands = createEmptyHands();

        const move: Move = {
            type: "move",
            from: sq(2, 2),
            to: sq(8, 8),
            piece: makePiece("角", "white"),
            promote: false,
            captured: makePiece("飛", "black"),
        };

        const applied = applyMove(board, hands, "white", move);
        expect(applied.hands.white.飛).toBe(1);
        expect(applied.board["88"]).toMatchObject({ kind: "角", owner: "white" });

        const reverted = revertMove(applied.board, applied.hands, applied.nextTurn, move);
        expect(reverted.hands.white.飛).toBe(0);
        expect(reverted.board["88"]).toMatchObject({ kind: "飛", owner: "black" });
    });

    it("打ち手を適用・巻き戻し", () => {
        const board: Board = { ...nullBoard };
        const hands = createEmptyHands();
        hands.black.歩 = 3;

        const dropMove: Move = {
            type: "drop",
            to: sq(5, 5),
            piece: makePiece("歩", "black"),
        };

        const applied = applyMove(board, hands, "black", dropMove);
        expect(applied.hands.black.歩).toBe(2);
        expect(applied.board["55"]).toMatchObject({ kind: "歩", owner: "black", promoted: false });

        const reverted = revertMove(applied.board, applied.hands, applied.nextTurn, dropMove);
        expect(reverted.hands.black.歩).toBe(3);
        expect(reverted.board["55"]).toBeNull();
    });
});

describe("generateMoves", () => {
    it("歩は前方に1マス進める", () => {
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(7, 5), makePiece("歩", "black"));

        const moves = generateMoves(board, sq(7, 5));
        expect(moves.length).toBe(1);
        expect(moves[0].to).toEqual(sq(6, 5));
    });

    it("角はスライド移動（8方向に複数マス）", () => {
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(5, 5), makePiece("角", "black"));

        const moves = generateMoves(board, sq(5, 5));
        const targets = moves.map((m) => `${m.to.row}${m.to.column}`);
        // 角道が通っている限り斜め移動可能
        expect(targets).toContain("44");
        expect(targets).toContain("33");
        expect(targets).toContain("22");
        expect(targets).toContain("11");
        expect(targets).toContain("88");
        expect(targets).toContain("19");
    });

    it("玉は8方向に1マス（後手の玉）", () => {
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(5, 5), makePiece("玉", "white"));

        const moves = generateMoves(board, sq(5, 5));
        const targets = moves.map((m) => `${m.to.row}${m.to.column}`);
        expect(targets.length).toBe(8);
        for (const t of ["44", "45", "46", "54", "56", "64", "65", "66"]) {
            expect(targets).toContain(t);
        }
    });
});

describe("hasNifuViolation", () => {
    it("同じ筋に歩がない場合はfalseを返す", () => {
        const board = nullBoard;
        expect(hasNifuViolation(board, 1, "black")).toBe(false);
    });

    it("同じ筋に自分の歩がある場合はtrueを返す", () => {
        let board = nullBoard;
        board = setPiece(board, sq(7, 1), makePiece("歩", "black"));
        expect(hasNifuViolation(board, 1, "black")).toBe(true);
    });

    it("同じ筋に相手の歩がある場合はfalseを返す", () => {
        let board = nullBoard;
        board = setPiece(board, sq(3, 1), makePiece("歩", "white"));
        expect(hasNifuViolation(board, 1, "black")).toBe(false);
    });

    it("同じ筋に成った歩（と金）がある場合はfalseを返す", () => {
        let board = nullBoard;
        board = setPiece(board, sq(3, 1), makePiece("歩", "black", true));
        expect(hasNifuViolation(board, 1, "black")).toBe(false);
    });
});

describe("isDeadPiece", () => {
    it("歩が先手の最奥段（1段目）に置かれる場合はtrue", () => {
        expect(isDeadPiece(makePiece("歩", "black"), 1)).toBe(true);
    });

    it("歩が後手の最奥段（9段目）に置かれる場合はtrue", () => {
        expect(isDeadPiece(makePiece("歩", "white"), 9)).toBe(true);
    });

    it("香が先手の最奥段（1段目）に置かれる場合はtrue", () => {
        expect(isDeadPiece(makePiece("香", "black"), 1)).toBe(true);
    });

    it("桂が先手の最奥2段（1,2段目）に置かれる場合はtrue", () => {
        expect(isDeadPiece(makePiece("桂", "black"), 1)).toBe(true);
        expect(isDeadPiece(makePiece("桂", "black"), 2)).toBe(true);
        expect(isDeadPiece(makePiece("桂", "black"), 3)).toBe(false);
    });

    it("桂が後手の最奥2段（8,9段目）に置かれる場合はtrue", () => {
        expect(isDeadPiece(makePiece("桂", "white"), 8)).toBe(true);
        expect(isDeadPiece(makePiece("桂", "white"), 9)).toBe(true);
        expect(isDeadPiece(makePiece("桂", "white"), 7)).toBe(false);
    });

    it("その他の駒は常にfalse", () => {
        expect(isDeadPiece(makePiece("金", "black"), 1)).toBe(false);
        expect(isDeadPiece(makePiece("銀", "black"), 1)).toBe(false);
        expect(isDeadPiece(makePiece("角", "black"), 1)).toBe(false);
        expect(isDeadPiece(makePiece("飛", "black"), 1)).toBe(false);
        expect(isDeadPiece(makePiece("王", "black"), 1)).toBe(false);
    });
});

describe("applyMove - 特殊ルール", () => {
    it("二歩の場合はエラーを投げる", () => {
        let board = nullBoard;
        board = setPiece(board, sq(7, 1), makePiece("歩", "black"));
        const hands = createEmptyHands();
        hands.black.歩 = 1;

        const dropMove: Move = {
            type: "drop",
            to: sq(5, 1),
            piece: makePiece("歩", "black"),
        };

        expect(() => applyMove(board, hands, "black", dropMove)).toThrow("二歩です");
    });

    it("行き所のない駒を打つ場合はエラーを投げる", () => {
        const board = nullBoard;
        const hands = createEmptyHands();
        hands.black.歩 = 1;

        const dropMove: Move = {
            type: "drop",
            to: sq(1, 5),
            piece: makePiece("歩", "black"),
        };

        expect(() => applyMove(board, hands, "black", dropMove)).toThrow("行き所のない駒です");
    });

    it("行き所のない位置に移動する場合は自動的に成る", () => {
        let board = nullBoard;
        board = setPiece(board, sq(2, 5), makePiece("歩", "black"));
        const hands = createEmptyHands();

        const move: Move = {
            type: "move",
            from: sq(2, 5),
            to: sq(1, 5),
            piece: makePiece("歩", "black"),
            promote: false, // 成らないを指定しても
            captured: null,
        };

        const result = applyMove(board, hands, "black", move);
        const movedPiece = result.board["15"];
        expect(movedPiece).not.toBeNull();
        expect(movedPiece?.promoted).toBe(true); // 強制的に成る
    });

    it("打ち歩詰めの場合はエラーを投げる", () => {
        // 詰み局面を作成（uchifuzume.test.tsと同じ）
        let board = nullBoard;
        board = setPiece(board, sq(1, 1), makePiece("玉", "white"));
        board = setPiece(board, sq(1, 2), makePiece("金", "black"));
        board = setPiece(board, sq(2, 2), makePiece("銀", "black"));
        board = setPiece(board, sq(3, 1), makePiece("金", "black"));
        board = setPiece(board, sq(1, 3), makePiece("銀", "black"));
        // 2一に歩を打つと詰みになる局面

        const hands = createEmptyHands();
        hands.black.歩 = 1;

        const dropMove: Move = {
            type: "drop",
            to: sq(2, 1), // ここに歩を打つと詰み
            piece: makePiece("歩", "black"),
        };

        expect(() => applyMove(board, hands, "black", dropMove)).toThrow("打ち歩詰めです");
    });
});

describe("isUchifuzume", () => {
    it("歩を打って詰みになる場合はtrueを返す", () => {
        let board = nullBoard;
        // 後手玉を1一に配置（uchifuzume.test.tsと同じ詰み局面）
        board = setPiece(board, sq(1, 1), makePiece("玉", "white"));
        board = setPiece(board, sq(1, 2), makePiece("金", "black")); // 1二
        board = setPiece(board, sq(2, 2), makePiece("銀", "black")); // 2二
        board = setPiece(board, sq(3, 1), makePiece("金", "black")); // 3一
        board = setPiece(board, sq(1, 3), makePiece("銀", "black")); // 1三
        // この状態で2一に歩を打つと詰み

        expect(isUchifuzume(board, sq(2, 1), "black")).toBe(true);
    });

    it("歩を打っても詰みにならない場合はfalseを返す", () => {
        let board = nullBoard;
        board = setPiece(board, sq(5, 5), makePiece("玉", "white"));

        expect(isUchifuzume(board, sq(6, 5), "black")).toBe(false);
    });
});
