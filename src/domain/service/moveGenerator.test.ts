import test from "node:test";
import assert from "node:assert/strict";
import { Board } from "../model/board";
import { Square } from "../model/square";
import { Piece } from "../model/piece";

// Utility: 空の盤面
const emptyBoard: Board = {};

// Utility: 駒配置ヘルパー
const place = (board: Board, square: Square, piece: Piece): Board => ({
    ...board,
    [`${square.row}${square.column}`]: piece,
});

test("returns no moves for empty board", () => {
    const moves = generateLegalMoves(emptyBoard, "black");
    assert.deepEqual(moves, []);
});

test("generates forward move for black 歩", () => {
    const board: Board = place(
        emptyBoard,
        { row: 5, column: 5 },
        {
            kind: "歩",
            promoted: false,
            owner: "black",
        },
    );

    const moves = generateLegalMoves(board, "black");

    assert.ok(
        moves.some(
            (m) =>
                m.from.row === 5 &&
                m.from.column === 5 &&
                m.to.row === 4 &&
                m.to.column === 5 &&
                m.promote === false,
        ),
    );
});

test("ignores blocked forward move", () => {
    let board: Board = {};
    board = place(
        board,
        { row: 5, column: 5 },
        {
            kind: "歩",
            promoted: false,
            owner: "black",
        },
    );
    board = place(
        board,
        { row: 4, column: 5 },
        {
            kind: "金",
            promoted: false,
            owner: "black",
        },
    );

    const moves = generateLegalMoves(board, "black");
    assert.ok(!moves.some((m) => m.to.row === 4 && m.to.column === 5));
});
function generateLegalMoves(emptyBoard: Board, arg1: string) {
    throw new Error("Function not implemented.");
}
