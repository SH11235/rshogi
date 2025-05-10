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
        let board: Board = {};
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
        const board: Board = {};
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
        let board: Board = {};
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
        let board: Board = {};
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
