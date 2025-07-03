import { initialHands, modernInitialBoard } from "shogi-core";
import type { Board, Hands, Move } from "shogi-core";
import { describe, expect, it } from "vitest";
import { AIEngine } from "./engine";
import { evaluatePosition } from "./evaluation";
import { IterativeDeepeningSearch, TranspositionTable, orderMoves } from "./search";

describe("TranspositionTable", () => {
    it("should store and retrieve entries", () => {
        const tt = new TranspositionTable();
        const board = modernInitialBoard;
        const hands = initialHands();
        const player = "black";

        const entry = {
            score: 100,
            depth: 5,
            type: "exact" as const,
        };

        tt.set(board, hands, player, entry);
        const retrieved = tt.get(board, hands, player);

        expect(retrieved).toEqual(entry);
    });

    it("should respect size limit", () => {
        const tt = new TranspositionTable(2); // Max 2 entries
        const board = modernInitialBoard;
        const hands = initialHands();

        tt.set(board, hands, "black", { score: 1, depth: 1, type: "exact" });
        tt.set(board, hands, "white", { score: 2, depth: 2, type: "exact" });
        tt.set({ ...board, "99": null }, hands, "black", { score: 3, depth: 3, type: "exact" });

        // 最初のエントリは削除されているはず
        expect(tt.get(board, hands, "black")).toBeUndefined();
        expect(tt.get(board, hands, "white")).toBeDefined();
    });
});

describe("orderMoves", () => {
    it("should prioritize PV move", () => {
        // 初期局面での実際の合法手
        const moves: Move[] = [
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
            {
                type: "move",
                from: { row: 7, column: 6 },
                to: { row: 6, column: 6 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
        ];

        const pvMove = moves[1]; // 2つ目の手をPVに
        const ordered = orderMoves(moves, modernInitialBoard, initialHands(), "black", pvMove);

        expect(ordered[0]).toEqual(pvMove);
    });

    it("should prioritize captures", () => {
        // 駒を取る手と取らない手
        const moves: Move[] = [
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: { type: "pawn", owner: "white", promoted: false },
            },
        ];

        const ordered = orderMoves(moves, modernInitialBoard, initialHands(), "black");
        // 駒を取る手が優先される
        expect(ordered[0].type === "move" && ordered[0].captured).toBeTruthy();
    });

    it("should prioritize promotions", () => {
        // 成りと成らない手（同じ移動）
        const moves: Move[] = [
            {
                type: "move",
                from: { row: 3, column: 3 },
                to: { row: 2, column: 3 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
            {
                type: "move",
                from: { row: 3, column: 3 },
                to: { row: 2, column: 3 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: true,
                captured: null,
            },
        ];

        const ordered = orderMoves(moves, modernInitialBoard, initialHands(), "black");
        // 成る手が優先される
        expect(ordered[0].type === "move" && ordered[0].promote).toBeTruthy();
    });
});

describe("IterativeDeepeningSearch", () => {
    it("should find a legal move", async () => {
        const search = new IterativeDeepeningSearch();
        const board = modernInitialBoard;
        const hands = initialHands();
        const player = "black";

        // 初期局面での実際の合法手（一部）
        const moves: Move[] = [
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
            {
                type: "move",
                from: { row: 7, column: 6 },
                to: { row: 6, column: 6 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
        ];

        const engine = new AIEngine("beginner");
        const searchOptions = {
            maxDepth: 2,
            timeLimit: 1000,
            evaluate: (b: Board, h: Hands, p: "black" | "white") => {
                const evaluation = evaluatePosition(b, h, p);
                return evaluation.total;
            },
            generateMoves: (b: Board, h: Hands, p: "black" | "white") => {
                // 実際のエンジンの手生成を使用
                return engine.generateAllLegalMoves(b, h, p);
            },
        };

        const result = await search.search(board, hands, player, moves, searchOptions);

        expect(result.bestMove).toBeDefined();
        expect(result.depth).toBeGreaterThan(0);
        expect(result.nodes).toBeGreaterThan(0);
    });

    it("should respect time limit", async () => {
        const search = new IterativeDeepeningSearch();
        const engine = new AIEngine("beginner");
        const board = modernInitialBoard;
        const hands = initialHands();
        const player = "black";

        // 初期局面の合法手を使用
        const moves = engine.generateAllLegalMoves(board, hands, player);

        const searchOptions = {
            maxDepth: 10,
            timeLimit: 100, // 100ms
            evaluate: (b: Board, h: Hands, p: "black" | "white") => {
                const evaluation = evaluatePosition(b, h, p);
                return evaluation.total;
            },
            generateMoves: (b: Board, h: Hands, p: "black" | "white") => {
                // 実際のエンジンの手生成を使用
                return engine.generateAllLegalMoves(b, h, p);
            },
        };

        const startTime = Date.now();
        const result = await search.search(board, hands, player, moves, searchOptions);
        const endTime = Date.now();

        expect(endTime - startTime).toBeLessThan(200); // タイムリミット＋余裕
        expect(result.time).toBeLessThanOrEqual(searchOptions.timeLimit + 100);
    });

    it("should improve depth with iterative deepening", async () => {
        const search = new IterativeDeepeningSearch();
        const board = modernInitialBoard;
        const hands = initialHands();
        const player = "black";

        const moves: Move[] = [
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
        ];

        const engine = new AIEngine("beginner");
        const searchOptions = {
            maxDepth: 3,
            timeLimit: 2000,
            evaluate: (b: Board, h: Hands, p: "black" | "white") => {
                const evaluation = evaluatePosition(b, h, p);
                return evaluation.total;
            },
            generateMoves: (b: Board, h: Hands, p: "black" | "white") => {
                // 実際のエンジンの手生成を使用
                return engine.generateAllLegalMoves(b, h, p);
            },
        };

        const result = await search.search(board, hands, player, moves, searchOptions);

        // 時間が十分ならより深く探索されるはず
        expect(result.depth).toBeGreaterThanOrEqual(2);
    });
});
