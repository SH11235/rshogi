import { describe, it, expect } from "vitest";
import { Board } from "../model/board";
import { Square } from "../model/square";
import { Piece } from "../model/piece";
import { generateMoves } from "./moveGenerator";

/** ---- ユーティリティ ---- */
const emptyBoard: Board = {};

const place = (board: Board, square: Square, piece: Piece): Board => ({
    ...board,
    [`${square.row}${square.column}`]: piece,
});

/** ---- テスト ---- */
describe("generateMoves", () => {
    it("returns no moves for empty board", () => {
        const moves = generateMoves(emptyBoard, "black");
        expect(moves).toEqual([]);
    });

    it("generates forward move for black 歩", () => {
        const board: Board = place(
            emptyBoard,
            { row: 5, column: 5 },
            { kind: "歩", promoted: false, owner: "black" },
        );

        const moves = generateMoves(board, "black");

        expect(
            moves.some(
                (m) =>
                    m.from.row === 5 &&
                    m.from.column === 5 &&
                    m.to.row === 4 &&
                    m.to.column === 5 &&
                    m.promote === false,
            ),
        ).toBe(true);
    });

    it("ignores blocked forward move", () => {
        let board: Board = {};
        board = place(
            board,
            { row: 5, column: 5 },
            { kind: "歩", promoted: false, owner: "black" },
        );
        board = place(
            board,
            { row: 4, column: 5 },
            { kind: "金", promoted: false, owner: "black" },
        );

        const moves = generateMoves(board, "black");
        expect(moves.some((m) => m.to.row === 4 && m.to.column === 5)).toBe(false);
    });
});
