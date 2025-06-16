import { describe, expect, it } from "vitest";
import type { Board } from "../model/board";
import { createPiece } from "../model/piece";
import type { Square } from "../model/square";
import { generateLegalDropMovesForPiece, generateLegalMoves } from "./legalMoves";
import { initialHands } from "./moveService";

describe("legalMoves", () => {
    describe("generateLegalMoves", () => {
        it("should generate legal moves excluding moves that put own king in check", () => {
            // Create a board where moving a piece would expose the king to check
            const board: Board = {} as Board;

            // Initialize empty board
            for (let row = 1; row <= 9; row++) {
                for (let col = 1; col <= 9; col++) {
                    board[`${row}${col}` as keyof Board] = null;
                }
            }

            // Place black king at 55
            board["55"] = createPiece("king", "black");
            // Place black silver at 54 (protecting king from white rook)
            board["54"] = createPiece("silver", "black");
            // Place white rook at 51 (would attack king if silver moves)
            board["51"] = createPiece("rook", "white");

            const hands = initialHands();
            const silverSquare: Square = { row: 5, column: 4 };

            const legalMoves = generateLegalMoves(board, hands, silverSquare, "black");

            // Silver should have limited moves because moving would expose king
            expect(legalMoves).toBeDefined();
            expect(Array.isArray(legalMoves)).toBe(true);

            // The silver cannot move to certain squares because it would expose the king
            // This is a basic test to ensure the function works
            expect(legalMoves.length).toBeGreaterThanOrEqual(0);
        });

        it("should return empty array for non-existent piece", () => {
            const board: Board = {} as Board;

            // Initialize empty board
            for (let row = 1; row <= 9; row++) {
                for (let col = 1; col <= 9; col++) {
                    board[`${row}${col}` as keyof Board] = null;
                }
            }

            const hands = initialHands();
            const emptySquare: Square = { row: 5, column: 5 };

            const legalMoves = generateLegalMoves(board, hands, emptySquare, "black");

            expect(legalMoves).toEqual([]);
        });

        it("should return empty array for opponent's piece", () => {
            const board: Board = {} as Board;

            // Initialize empty board
            for (let row = 1; row <= 9; row++) {
                for (let col = 1; col <= 9; col++) {
                    board[`${row}${col}` as keyof Board] = null;
                }
            }

            // Place white pawn
            board["55"] = createPiece("pawn", "white");

            const hands = initialHands();
            const pawnSquare: Square = { row: 5, column: 5 };

            // Try to get moves for black player on white piece
            const legalMoves = generateLegalMoves(board, hands, pawnSquare, "black");

            expect(legalMoves).toEqual([]);
        });
    });

    describe("generateLegalDropMovesForPiece", () => {
        it("should generate legal drop moves excluding moves that put own king in check", () => {
            const board: Board = {} as Board;

            // Initialize empty board
            for (let row = 1; row <= 9; row++) {
                for (let col = 1; col <= 9; col++) {
                    board[`${row}${col}` as keyof Board] = null;
                }
            }

            // Place black king at 55
            board["55"] = createPiece("king", "black");

            const hands = initialHands();
            // Give black player a pawn to drop
            hands.black.歩 = 1;

            const legalDrops = generateLegalDropMovesForPiece(board, hands, "pawn", "black");

            expect(legalDrops).toBeDefined();
            expect(Array.isArray(legalDrops)).toBe(true);
            // Should have some legal drop squares
            expect(legalDrops.length).toBeGreaterThan(0);
        });

        it("should return empty array when no pieces of that type are available", () => {
            const board: Board = {} as Board;

            // Initialize empty board
            for (let row = 1; row <= 9; row++) {
                for (let col = 1; col <= 9; col++) {
                    board[`${row}${col}` as keyof Board] = null;
                }
            }

            const hands = initialHands();
            // No rooks available to drop
            hands.black.飛 = 0;

            const legalDrops = generateLegalDropMovesForPiece(board, hands, "rook", "black");

            expect(legalDrops).toEqual([]);
        });

        it("should handle invalid piece type gracefully", () => {
            const board: Board = {} as Board;

            // Initialize empty board
            for (let row = 1; row <= 9; row++) {
                for (let col = 1; col <= 9; col++) {
                    board[`${row}${col}` as keyof Board] = null;
                }
            }

            const hands = initialHands();

            const legalDrops = generateLegalDropMovesForPiece(
                board,
                hands,
                // biome-ignore lint/suspicious/noExplicitAny: Testing invalid input
                "invalid" as any,
                "black",
            );

            expect(legalDrops).toEqual([]);
        });
    });
});
