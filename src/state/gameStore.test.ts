import type { Board } from "@/domain/model/board";
import type { Move } from "@/domain/model/move";
import type { Piece } from "@/domain/model/piece";
import { createEmptyHands } from "@/domain/service/moveService";
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
        board = place(board, "55", { kind: "王", promoted: false, owner: "black" });
        board = place(board, "45", { kind: "飛", promoted: false, owner: "white" });
        board = place(board, "54", { kind: "銀", promoted: false, owner: "white" });
        board = place(board, "56", { kind: "銀", promoted: false, owner: "white" });
        board = place(board, "65", { kind: "桂", promoted: false, owner: "white" });
        board = place(board, "44", { kind: "金", promoted: false, owner: "white" });
        board = place(board, "46", { kind: "金", promoted: false, owner: "white" });
        board = place(board, "64", { kind: "金", promoted: false, owner: "white" });
        board = place(board, "66", { kind: "金", promoted: false, owner: "white" });

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
            piece: { kind: "金", promoted: false, owner: "white" },
        };

        useGameStore.getState().makeMove(move);

        expect(useGameStore.getState().result).toEqual({
            winner: "white",
            reason: "checkmate",
        });
    });
});
