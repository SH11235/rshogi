import { initialHands, modernInitialBoard } from "shogi-core";
import type { Column, Row } from "shogi-core";
import { describe, expect, it } from "vitest";
import { OpeningBook } from "./openingBook";
import { generateMainOpenings } from "./openingGenerator";

describe("OpeningBook", () => {
    it("should create an empty opening book", () => {
        const book = new OpeningBook();
        expect(book.size()).toBe(0);
    });

    it("should add and retrieve entries", () => {
        const book = new OpeningBook();
        const board = modernInitialBoard;
        const hands = initialHands();

        const moves = [
            {
                move: {
                    type: "move" as const,
                    from: { row: 7 as Row, column: 7 as Column },
                    to: { row: 7 as Row, column: 6 as Column },
                    piece: { type: "pawn" as const, owner: "black" as const, promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 50,
                name: "居飛車",
            },
        ];

        book.addEntry(board, hands, moves, 0);
        expect(book.size()).toBe(1);
        expect(book.hasPosition(board, hands)).toBe(true);
    });

    it("should return null for unknown positions", () => {
        const book = new OpeningBook();
        const board = modernInitialBoard;
        const hands = initialHands();

        const move = book.getMove(board, hands, 0);
        expect(move).toBeNull();
    });

    it("should respect max depth", () => {
        const book = new OpeningBook(10);
        const board = modernInitialBoard;
        const hands = initialHands();

        const moves = [
            {
                move: {
                    type: "move" as const,
                    from: { row: 7 as Row, column: 7 as Column },
                    to: { row: 7 as Row, column: 6 as Column },
                    piece: { type: "pawn" as const, owner: "black" as const, promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 50,
            },
        ];

        // 深さ制限内
        book.addEntry(board, hands, moves, 5);
        expect(book.size()).toBe(1);

        // 深さ制限を超える
        book.addEntry(board, hands, moves, 15);
        expect(book.size()).toBe(1); // 追加されない
    });

    it("should select moves based on weight", () => {
        const book = new OpeningBook();
        const board = modernInitialBoard;
        const hands = initialHands();

        const highWeightMove = {
            type: "move" as const,
            from: { row: 7 as Row, column: 7 as Column },
            to: { row: 7 as Row, column: 6 as Column },
            piece: { type: "pawn" as const, owner: "black" as const, promoted: false },
            promote: false,
            captured: null,
        };

        const lowWeightMove = {
            type: "move" as const,
            from: { row: 2 as Row, column: 6 as Column },
            to: { row: 2 as Row, column: 5 as Column },
            piece: { type: "pawn" as const, owner: "black" as const, promoted: false },
            promote: false,
            captured: null,
        };

        const moves = [
            { move: highWeightMove, weight: 90 },
            { move: lowWeightMove, weight: 10 },
        ];

        book.addEntry(board, hands, moves, 0);

        // 多数回試行して、高い重みの手が多く選ばれることを確認
        let highWeightCount = 0;
        const trials = 1000;

        for (let i = 0; i < trials; i++) {
            const selected = book.getMove(board, hands, 0);
            if (selected === highWeightMove) {
                highWeightCount++;
            }
        }

        // 90%の重みなので、約900回選ばれるはず（許容誤差あり）
        expect(highWeightCount).toBeGreaterThan(800);
        expect(highWeightCount).toBeLessThan(950);
    });

    it("should load opening data", () => {
        const book = new OpeningBook();
        const openingData = generateMainOpenings();

        book.loadFromData(openingData);
        expect(book.size()).toBeGreaterThan(0);

        // 初期局面の定跡が含まれているか確認
        const board = modernInitialBoard;
        const hands = initialHands();
        const moves = book.getMoves(board, hands);

        expect(moves.length).toBeGreaterThan(0);
        expect(moves.some((m) => m.name === "居飛車")).toBe(true);
    });

    it("should get all moves for a position", () => {
        const book = new OpeningBook();
        const board = modernInitialBoard;
        const hands = initialHands();

        const moves = [
            {
                move: {
                    type: "move" as const,
                    from: { row: 7 as Row, column: 7 as Column },
                    to: { row: 7 as Row, column: 6 as Column },
                    piece: { type: "pawn" as const, owner: "black" as const, promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 50,
                name: "居飛車",
                comment: "最も一般的",
            },
            {
                move: {
                    type: "move" as const,
                    from: { row: 2 as Row, column: 6 as Column },
                    to: { row: 2 as Row, column: 5 as Column },
                    piece: { type: "pawn" as const, owner: "black" as const, promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 30,
                name: "相掛かり",
                comment: "積極的",
            },
        ];

        book.addEntry(board, hands, moves, 0);
        const retrievedMoves = book.getMoves(board, hands);

        expect(retrievedMoves).toHaveLength(2);
        expect(retrievedMoves[0].name).toBe("居飛車");
        expect(retrievedMoves[1].name).toBe("相掛かり");
    });
});
