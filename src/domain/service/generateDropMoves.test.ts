import { describe, expect, it } from "vitest";
import type { Board } from "@/domain/model/board";
import { setPiece } from "@/domain/model/board";
import type { Piece } from "@/domain/model/piece";
import type { Column, Row, Square } from "@/domain/model/square";
import { generateDropMoves } from "@/domain/service/generateDropMoves";

const sq = (row: Row, col: Column): Square => ({ row, column: col });

const makePiece = (
    kind: Piece["kind"],
    owner: Piece["owner"],
    promoted = false,
): Piece => ({
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

describe("generateDropMoves", () => {
    it("dropping on empty square yields a DropMove", () => {
        const board: Board = { ...nullBoard };
        const moves = generateDropMoves(board, "black", "銀");
        expect(moves).toContainEqual({
            type: "drop",
            to: sq(5, 5),
            piece: makePiece("銀", "black"),
        });
    });

    it("nifu prevents pawn drop in occupied column", () => {
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(7, 5), makePiece("歩", "black"));
        const moves = generateDropMoves(board, "black", "歩");
        expect(moves.some((m) => m.to.column === 5)).toBe(false);
    });

    it("other piece drops unaffected by nifu", () => {
        let board: Board = { ...nullBoard };
        board = setPiece(board, sq(7, 5), makePiece("歩", "black"));
        const moves = generateDropMoves(board, "black", "角");
        expect(moves.some((m) => m.to.column === 5)).toBe(true);
    });
});
