import test from "node:test";
import assert from "node:assert/strict";

import { isCheckmate } from "../../../src/domain/service/checkmate";
import { Board } from "../../../src/domain/model/board";
import { Piece } from "../../../src/domain/model/piece";
import { Square } from "../../../src/domain/model/square";

// 駒配置ユーティリティ
const place = (board: Board, square: Square, piece: Piece): Board => ({
    ...board,
    [`${square.row}${square.column}`]: piece,
});

test("王手されていない場合は詰みでない", () => {
    const board: Board = {
        "55": {
            kind: "王",
            promoted: false,
            owner: "black",
        },
        "11": {
            kind: "歩",
            promoted: false,
            owner: "white",
        },
    };

    const result = isCheckmate(board, "black");
    assert.equal(result, false);
});

test("王の逃げ道がある場合は詰みでない", () => {
    let board: Board = {};
    board = place(
        board,
        { row: 5, column: 5 },
        {
            kind: "王",
            promoted: false,
            owner: "black",
        },
    );
    board = place(
        board,
        { row: 6, column: 5 },
        {
            kind: "飛",
            promoted: false,
            owner: "white",
        },
    );

    const result = isCheckmate(board, "black");
    assert.equal(result, false); // 王は左や右に逃げられる
});

test("逃げ道がなく、詰みである場合", () => {
    let board: Board = {};
    board = place(
        board,
        { row: 5, column: 5 },
        {
            kind: "王",
            promoted: false,
            owner: "black",
        },
    );
    board = place(
        board,
        { row: 4, column: 5 },
        {
            kind: "飛",
            promoted: false,
            owner: "white",
        },
    );
    board = place(
        board,
        { row: 5, column: 4 },
        {
            kind: "銀",
            promoted: false,
            owner: "white",
        },
    );
    board = place(
        board,
        { row: 5, column: 6 },
        {
            kind: "銀",
            promoted: false,
            owner: "white",
        },
    );

    const result = isCheckmate(board, "black");
    assert.equal(result, true); // 王がどこにも動けない
});
