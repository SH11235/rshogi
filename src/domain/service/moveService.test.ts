import type { Board } from "@/domain/model/board";
import { setPiece } from "@/domain/model/board";
import type { Move } from "@/domain/model/move";
import type { Piece } from "@/domain/model/piece";
import { promote } from "@/domain/model/piece";
import {
    applyMove,
    createEmptyHands,
    generateMoves,
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
    it("toggles black ↔︎ white", () => {
        expect(toggleSide("black")).toBe("white");
        expect(toggleSide("white")).toBe("black");
    });
});

describe("applyMove / revertMove", () => {
    it("applies a normal move with capture and promotion, then reverts", () => {
        // initial board: 黒の桂馬が 7-7, 白の歩が 6-5
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(7, 7), makePiece("桂", "black"));
        board = setPiece(board, sq(6, 5), makePiece("歩", "white"));

        const hands = createEmptyHands();

        const move: Move = {
            type: "move",
            from: sq(7, 7),
            to: sq(6, 5),
            piece: makePiece("桂", "black"),
            promote: true,
            captured: makePiece("歩", "white"),
        };

        const {
            board: afterBoard,
            hands: afterHands,
            nextTurn,
        } = applyMove(board, hands, "black", move);

        // 桂馬が成りで6-5にある
        expect(afterBoard["65"]).toEqual(promote(makePiece("桂", "black")));
        // 先手の持ち駒に歩が加算
        expect(afterHands.black.歩).toBe(1);
        expect(nextTurn).toBe("white");

        // Revert and confirm board/hands identical to start
        const {
            board: reverted,
            hands: revertedHands,
            nextTurn: revertedTurn,
        } = revertMove(afterBoard, afterHands, nextTurn, move);
        expect(reverted["77"]).toEqual(makePiece("桂", "black"));
        expect(reverted["65"]).toEqual(makePiece("歩", "white"));
        expect(revertedHands.black.歩).toBe(0);
        expect(revertedTurn).toBe("black");
    });

    it("applies a drop move and reverts", () => {
        const board: Board = { ...nullBoard };
        const hands = createEmptyHands();
        hands.black.銀 = 1; // 先手が銀を1枚持っている

        const drop: Move = {
            type: "drop",
            to: sq(5, 5),
            piece: makePiece("銀", "black"),
        };

        const { board: after, hands: hAfter } = applyMove(board, hands, "black", drop);

        expect(after["55"]).toEqual(makePiece("銀", "black"));
        expect(hAfter.black.銀).toBe(0);

        const { board: rev, hands: hRev } = revertMove(after, hAfter, "white", drop);
        expect(rev["55"]).toBeNull();
        expect(hRev.black.銀).toBe(1);
    });
});

describe("generateMoves", () => {
    it("generates pawn forward move but blocks on own piece", () => {
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(7, 5), makePiece("歩", "black"));
        // 自駒が前にいる場合
        board = setPiece(board, sq(6, 5), makePiece("金", "black"));

        const movesBlocked = generateMoves(board, sq(7, 5));
        expect(movesBlocked.length).toBe(0);

        // ブロックを外すと1手生成
        board = setPiece(board, sq(6, 5), null);
        const moves = generateMoves(board, sq(7, 5));
        expect(moves.length).toBe(1);
        expect(moves[0].to).toEqual(sq(6, 5));
    });

    it("bishop slides until piece encountered", () => {
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(5, 5), makePiece("角", "black"));
        board = setPiece(board, sq(3, 3), makePiece("歩", "black")); // 自駒ブロック
        board = setPiece(board, sq(7, 7), makePiece("歩", "white")); // 敵駒キャプチャ

        const moves = generateMoves(board, sq(5, 5));
        // 経路上 4,4 は生成されるが 3,3 でブロック、7,7 で止まる
        const targets = moves.map((m) => `${m.to.row}${m.to.column}`);
        expect(targets).toContain("44");
        expect(targets).not.toContain("33");
        expect(targets).toContain("77");
    });
});
