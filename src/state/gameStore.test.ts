import { describe, it, expect } from "vitest";
import type { Board } from "@/domain/model/board";
import type { Move } from "@/domain/model/move";
import type { Piece } from "@/domain/model/piece";
import { createEmptyHands } from "@/domain/service/moveService";
import { useGameStore } from "./gameStore";

// 簡易盤面設定ユーティリティ
const place = (board: Board, key: string, piece: Piece): Board => ({
    ...board,
    [key]: piece,
});

describe("useGameStore makeMove", () => {
    it("updates result when move results in checkmate", () => {
        let board: Board = {};
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
        hands.white.歩 = 1;

        useGameStore.setState({
            board,
            hands,
            history: [],
            cursor: -1,
            turn: "white",
            result: null,
        });

        const move: Move = {
            type: "drop",
            to: { row: 1, column: 1 },
            piece: { kind: "歩", promoted: false, owner: "white" },
        };

        useGameStore.getState().makeMove(move);

        expect(useGameStore.getState().result).toEqual({
            winner: "white",
            reason: "checkmate",
        });
    });
});
