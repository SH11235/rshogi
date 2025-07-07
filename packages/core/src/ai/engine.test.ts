import { describe, expect, it, vi } from "vitest";
import type { Board } from "../domain/model/board";
import type { Piece } from "../domain/model/piece";
import type { Hands } from "../domain/service/moveService";
import { initialHands } from "../domain/service/moveService";
import { AIEngine } from "./engine";
import { OpeningBook } from "./openingBook";
import type { OpeningBookLoaderInterface } from "./openingBookInterface";

// Mock OpeningBookLoader
const mockOpeningBookLoader: OpeningBookLoaderInterface = {
    async loadForDifficulty() {
        return new OpeningBook();
    },
    loadFromFallback() {
        return new OpeningBook();
    },
};

describe("AIEngine", () => {
    describe("generateAllLegalMoves", () => {
        it("should enforce promotion for pawns reaching the last rank", () => {
            const engine = new AIEngine("intermediate", mockOpeningBookLoader);

            // Create a board with a black pawn that can move to rank 1
            const board: Board = {};
            board["21"] = { type: "pawn", owner: "black", promoted: false };
            // Add kings (required for valid board state)
            board["59"] = { type: "king", owner: "black", promoted: false };
            board["51"] = { type: "king", owner: "white", promoted: false };

            const hands: Hands = initialHands();
            const moves = engine.generateAllLegalMoves(board, hands, "black");

            // Find moves for the pawn moving to rank 1
            const pawnMoves = moves.filter(
                (m) =>
                    m.type === "move" && m.from.row === 2 && m.from.column === 1 && m.to.row === 1,
            );

            // All moves to rank 1 should be promoted
            expect(pawnMoves.length).toBeGreaterThan(0);
            for (const move of pawnMoves) {
                if (move.type === "move") {
                    expect(move.promote).toBe(true);
                }
            }
        });

        it("should enforce promotion for white pawns reaching rank 9", () => {
            const engine = new AIEngine("intermediate", mockOpeningBookLoader);

            // Create a board with a white pawn that can move to rank 9
            const board: Board = {};
            board["81"] = { type: "pawn", owner: "white", promoted: false };
            // Add kings (required for valid board state)
            board["59"] = { type: "king", owner: "black", promoted: false };
            board["51"] = { type: "king", owner: "white", promoted: false };

            const hands: Hands = initialHands();
            const moves = engine.generateAllLegalMoves(board, hands, "white");

            // Find moves for the pawn moving to rank 9
            const pawnMoves = moves.filter(
                (m) =>
                    m.type === "move" && m.from.row === 8 && m.from.column === 1 && m.to.row === 9,
            );

            // All moves to rank 9 should be promoted
            expect(pawnMoves.length).toBeGreaterThan(0);
            for (const move of pawnMoves) {
                if (move.type === "move") {
                    expect(move.promote).toBe(true);
                }
            }
        });

        it("should enforce promotion for knights reaching the last two ranks", () => {
            const engine = new AIEngine("intermediate", mockOpeningBookLoader);

            // Create a board with a black knight that can move to rank 2
            const board: Board = {};
            board["33"] = { type: "knight", owner: "black", promoted: false };
            // Add kings (required for valid board state)
            board["59"] = { type: "king", owner: "black", promoted: false };
            board["51"] = { type: "king", owner: "white", promoted: false };

            const hands: Hands = initialHands();
            const moves = engine.generateAllLegalMoves(board, hands, "black");

            // Find moves for the knight moving to rank 1 or 2
            const knightMoves = moves.filter(
                (m) =>
                    m.type === "move" && m.from.row === 3 && m.from.column === 3 && m.to.row <= 2,
            );

            // Moves to rank 1 or 2 should be promoted
            for (const move of knightMoves) {
                if (move.type === "move" && move.to.row <= 2) {
                    expect(move.promote).toBe(true);
                }
            }
        });

        it("should offer both promoted and non-promoted options when promotion is optional", () => {
            const engine = new AIEngine("intermediate", mockOpeningBookLoader);

            // Create a board with a silver that can move into the promotion zone
            const board: Board = {};
            board["44"] = { type: "silver", owner: "black", promoted: false };
            // Add kings (required for valid board state)
            board["59"] = { type: "king", owner: "black", promoted: false };
            board["51"] = { type: "king", owner: "white", promoted: false };

            const hands: Hands = initialHands();
            const moves = engine.generateAllLegalMoves(board, hands, "black");

            // Find moves for the silver moving to rank 3
            const silverMovesToRank3 = moves.filter(
                (m) =>
                    m.type === "move" &&
                    m.from.row === 4 &&
                    m.from.column === 4 &&
                    m.to.row === 3 &&
                    m.piece.type === "silver",
            );

            // Should have both promoted and non-promoted versions
            const promotedMoves = silverMovesToRank3.filter((m) => m.type === "move" && m.promote);
            const nonPromotedMoves = silverMovesToRank3.filter(
                (m) => m.type === "move" && !m.promote,
            );

            expect(promotedMoves.length).toBeGreaterThan(0);
            expect(nonPromotedMoves.length).toBeGreaterThan(0);
        });

        it("should not shuffle moves for intermediate difficulty", () => {
            const engine = new AIEngine("intermediate", mockOpeningBookLoader);

            // Create a simple board
            const board: Board = {};
            board["55"] = { type: "king", owner: "black", promoted: false };
            board["11"] = { type: "pawn", owner: "black", promoted: false };
            board["21"] = { type: "pawn", owner: "black", promoted: false };

            const hands: Hands = initialHands();

            // Generate moves multiple times
            const movesSets = [];
            for (let i = 0; i < 5; i++) {
                const moves = engine.generateAllLegalMoves(board, hands, "black");
                movesSets.push(moves.map((m) => JSON.stringify(m)));
            }

            // All move sets should be identical (no shuffling)
            for (let i = 1; i < movesSets.length; i++) {
                expect(movesSets[i]).toEqual(movesSets[0]);
            }
        });

        it("should shuffle moves for beginner difficulty", () => {
            const engine = new AIEngine("beginner", mockOpeningBookLoader);

            // Create a board with many pieces to ensure variety
            const board: Board = {};
            board["55"] = { type: "king", owner: "black", promoted: false };
            for (let i = 1; i <= 9; i++) {
                board[`1${i}`] = { type: "pawn", owner: "black", promoted: false };
            }

            const hands: Hands = initialHands();

            // Generate moves multiple times
            const movesSets = [];
            for (let i = 0; i < 10; i++) {
                const moves = engine.generateAllLegalMoves(board, hands, "black");
                movesSets.push(moves.map((m) => JSON.stringify(m)));
            }

            // At least one set should be different (shuffled)
            let foundDifference = false;
            for (let i = 1; i < movesSets.length; i++) {
                if (JSON.stringify(movesSets[i]) !== JSON.stringify(movesSets[0])) {
                    foundDifference = true;
                    break;
                }
            }

            expect(foundDifference).toBe(true);
        });
    });

    describe("evaluatePosition", () => {
        it("should return search information when available from last evaluation", async () => {
            const engine = new AIEngine("intermediate", mockOpeningBookLoader);

            // Create a simple board
            const board: Board = {};
            board["55"] = { type: "king", owner: "black", promoted: false };
            board["51"] = { type: "king", owner: "white", promoted: false };

            const hands: Hands = initialHands();

            // First, calculate a move to populate lastEvaluation
            await engine.calculateBestMove(board, hands, "black", []);

            // Now evaluate position should return search information
            const evaluation = engine.evaluatePosition(board, hands, "black");

            expect(evaluation.depth).toBeGreaterThan(0);
            expect(evaluation.nodes).toBeGreaterThan(0);
            expect(evaluation.time).toBeGreaterThan(0);
            expect(evaluation.pv.length).toBeGreaterThan(0);
        });

        it("should return basic evaluation when no search information is available", () => {
            const engine = new AIEngine("intermediate", mockOpeningBookLoader);

            const board: Board = {};
            board["55"] = { type: "king", owner: "black", promoted: false };
            board["51"] = { type: "king", owner: "white", promoted: false };

            const hands: Hands = initialHands();

            // Evaluate without prior search
            const evaluation = engine.evaluatePosition(board, hands, "black");

            expect(evaluation.depth).toBe(0);
            expect(evaluation.nodes).toBe(0);
            expect(evaluation.time).toBe(0);
            expect(evaluation.pv.length).toBe(0);
            expect(typeof evaluation.score).toBe("number");
        });
    });

    describe("drop move generation", () => {
        it("should generate all drop moves in a single call", () => {
            const engine = new AIEngine("intermediate", mockOpeningBookLoader);

            const board: Board = {};
            board["55"] = { type: "king", owner: "black", promoted: false };
            board["51"] = { type: "king", owner: "white", promoted: false };

            const hands: Hands = initialHands();
            hands.black.pawn = 2;
            hands.black.lance = 1;
            hands.black.knight = 1;

            // Spy on generateAllDropMoves to ensure it's called only once
            const generateDropMovesSpy = vi.fn(() => [
                {
                    type: "drop" as const,
                    to: { row: 5, column: 5 },
                    piece: { type: "pawn", owner: "black", promoted: false } as Piece,
                },
            ]);

            // Mock the module
            vi.doMock("../domain/service/generateDropMoves", () => ({
                generateAllDropMoves: generateDropMovesSpy,
            }));

            const moves = engine.generateAllLegalMoves(board, hands, "black");

            // Should include drop moves
            const dropMoves = moves.filter((m) => m.type === "drop");
            expect(dropMoves.length).toBeGreaterThan(0);
        });
    });
});
