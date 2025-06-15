import { type Board, type Move, type Piece, createEmptyHands } from "shogi-core";
import { describe, expect, it } from "vitest";
import { useGameStore } from "./gameStore";

// 簡易盤面設定ユーティリティ
const place = (board: Board, key: string, piece: Piece): Board => ({
    ...board,
    [key]: piece,
});

describe("useGameStore makeMove", () => {
    it("updates result when move results in checkmate", () => {
        let board: Board = {} as Board;
        board = place(board, "55", { type: "king", promoted: false, owner: "black" });
        board = place(board, "45", { type: "rook", promoted: false, owner: "white" });
        board = place(board, "54", { type: "silver", promoted: false, owner: "white" });
        board = place(board, "56", { type: "silver", promoted: false, owner: "white" });
        board = place(board, "65", { type: "knight", promoted: false, owner: "white" });
        board = place(board, "44", { type: "gold", promoted: false, owner: "white" });
        board = place(board, "46", { type: "gold", promoted: false, owner: "white" });
        board = place(board, "64", { type: "gold", promoted: false, owner: "white" });
        board = place(board, "66", { type: "gold", promoted: false, owner: "white" });

        const hands = createEmptyHands();

        useGameStore.setState({
            board,
            hands,
            history: [],
            cursor: -1,
            turn: "white",
            result: null,
        });

        // 歩ではなく金で詰ませる（打ち歩詰めではない）
        hands.white.歩 = 0;
        hands.white.金 = 1;

        const move: Move = {
            type: "drop",
            to: { row: 1, column: 1 },
            piece: { type: "gold", promoted: false, owner: "white" },
        };

        useGameStore.getState().makeMove(move);

        expect(useGameStore.getState().result).toEqual({
            winner: "white",
            reason: "checkmate",
        });
    });
});
