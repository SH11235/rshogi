import { describe, expect, it } from "vitest";
import type { Board } from "../model/board";
import type { Piece } from "../model/piece";
import type { Square } from "../model/square";
import { isCheckmate } from "./checkmate";

// 駒配置ユーティリティ
const place = (board: Board, square: Square, piece: Piece): Board => ({
    ...board,
    [`${square.row}${square.column}`]: piece,
});

describe("Checkmate detection tests", () => {
    it("王手されていない場合は詰みでない", () => {
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
        expect(result).toBe(false);
    });

    it("王の逃げ道がある場合は詰みでない", () => {
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
        expect(result).toBe(false); // 王は左や右に逃げられる
    });

    it("逃げ道がなく、詰みである場合", () => {
        let board: Board = {};
        board = place(
            board,
            { row: 5, column: 5 },
            { kind: "王", promoted: false, owner: "black" },
        );
        board = place(
            board,
            { row: 4, column: 5 },
            { kind: "飛", promoted: false, owner: "white" },
        ); // 上
        board = place(
            board,
            { row: 5, column: 4 },
            { kind: "銀", promoted: false, owner: "white" },
        ); // 左
        board = place(
            board,
            { row: 5, column: 6 },
            { kind: "銀", promoted: false, owner: "white" },
        ); // 右
        board = place(
            board,
            { row: 6, column: 5 },
            { kind: "桂", promoted: false, owner: "white" },
        ); // 下
        board = place(
            board,
            { row: 4, column: 4 },
            { kind: "金", promoted: false, owner: "white" },
        ); // 左上
        board = place(
            board,
            { row: 4, column: 6 },
            { kind: "金", promoted: false, owner: "white" },
        ); // 右上
        board = place(
            board,
            { row: 6, column: 4 },
            { kind: "金", promoted: false, owner: "white" },
        ); // 左下
        board = place(
            board,
            { row: 6, column: 6 },
            { kind: "金", promoted: false, owner: "white" },
        ); // 右下

        const result = isCheckmate(board, "black");
        expect(result).toBe(true); // 王がどこにも動けない
    });
});
