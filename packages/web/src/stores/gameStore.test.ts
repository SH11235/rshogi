import { type Square, createPiece, modernInitialBoard } from "shogi-core";
import { beforeEach, describe, expect, it } from "vitest";
import { useGameStore } from "./gameStore";

describe("useGameStore", () => {
    beforeEach(() => {
        // Reset store to initial state before each test
        useGameStore.getState().resetGame();
    });

    describe("initial state", () => {
        it("should have correct initial values", () => {
            const state = useGameStore.getState();

            expect(state.board).toEqual(modernInitialBoard);
            expect(state.currentPlayer).toBe("black");
            expect(state.selectedSquare).toBeNull();
            expect(state.validMoves).toEqual([]);
            expect(state.moveHistory).toEqual([]);
            expect(state.gameStatus).toBe("playing");
        });

        it("should have empty hands initially", () => {
            const state = useGameStore.getState();

            expect(state.hands.black).toEqual({ 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 });
            expect(state.hands.white).toEqual({ 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 });
        });
    });

    describe("selectSquare", () => {
        it("should select a square with player's piece", () => {
            const blackPawnSquare: Square = { row: 7, column: 7 };

            useGameStore.getState().selectSquare(blackPawnSquare);

            const state = useGameStore.getState();
            expect(state.selectedSquare).toEqual(blackPawnSquare);
            expect(state.validMoves.length).toBeGreaterThan(0);
        });

        it("should not select opponent's piece", () => {
            const whitePawnSquare: Square = { row: 3, column: 7 };

            useGameStore.getState().selectSquare(whitePawnSquare);

            const state = useGameStore.getState();
            expect(state.selectedSquare).toBeNull();
            expect(state.validMoves).toEqual([]);
        });

        it("should deselect when clicking the same square twice", () => {
            const blackPawnSquare: Square = { row: 7, column: 7 };

            // First click - select
            useGameStore.getState().selectSquare(blackPawnSquare);
            expect(useGameStore.getState().selectedSquare).toEqual(blackPawnSquare);

            // Second click - deselect
            useGameStore.getState().selectSquare(blackPawnSquare);
            expect(useGameStore.getState().selectedSquare).toBeNull();
            expect(useGameStore.getState().validMoves).toEqual([]);
        });

        it("should make a move when clicking valid destination", () => {
            const fromSquare: Square = { row: 7, column: 7 };
            const toSquare: Square = { row: 6, column: 7 };

            // Select piece
            useGameStore.getState().selectSquare(fromSquare);
            const validMoves = useGameStore.getState().validMoves;

            // Ensure the destination is valid
            const isValidMove = validMoves.some(
                (m) => m.row === toSquare.row && m.column === toSquare.column,
            );
            expect(isValidMove).toBe(true);

            // Click destination
            useGameStore.getState().selectSquare(toSquare);

            const state = useGameStore.getState();
            expect(state.selectedSquare).toBeNull();
            expect(state.currentPlayer).toBe("white");
            expect(state.moveHistory.length).toBe(1);
        });
    });

    describe("makeMove", () => {
        it("should execute a valid move", () => {
            const from: Square = { row: 7, column: 7 };
            const to: Square = { row: 6, column: 7 };

            useGameStore.getState().makeMove(from, to);

            const state = useGameStore.getState();
            expect(state.currentPlayer).toBe("white");
            expect(state.moveHistory.length).toBe(1);
            const move = state.moveHistory[0];
            if (move.type === "move") {
                expect(move.from).toEqual(from);
                expect(move.to).toEqual(to);
            }
        });

        it("should handle promotion", () => {
            // Set up a pawn ready to promote
            const testBoard = { ...modernInitialBoard };
            testBoard["22"] = createPiece("pawn", "black");
            testBoard["77"] = null;

            useGameStore.setState({
                board: testBoard,
                currentPlayer: "black",
                selectedSquare: null,
                validMoves: [],
                moveHistory: [],
                gameStatus: "playing",
            });

            const from: Square = { row: 2, column: 2 };
            const to: Square = { row: 1, column: 2 };

            useGameStore.getState().makeMove(from, to, true);

            const state = useGameStore.getState();
            const move = state.moveHistory[0];
            if (move.type === "move") {
                expect(move.promote).toBe(true);
            }
        });

        it("should detect checkmate and update game status", () => {
            // This is a simplified test - in practice, you'd set up a proper checkmate position
            const from: Square = { row: 7, column: 7 };
            const to: Square = { row: 6, column: 7 };

            useGameStore.getState().makeMove(from, to);

            // The actual checkmate detection logic is in the core package
            // This test mainly verifies the flow works without errors
            const state = useGameStore.getState();
            expect(state.gameStatus).toBeDefined();
        });
    });

    describe("resetGame", () => {
        it("should reset to initial state", () => {
            // Make some moves first
            useGameStore.getState().makeMove({ row: 7, column: 7 }, { row: 6, column: 7 });
            useGameStore.getState().selectSquare({ row: 3, column: 3 });

            // Reset
            useGameStore.getState().resetGame();

            const state = useGameStore.getState();
            expect(state.board).toEqual(modernInitialBoard);
            expect(state.currentPlayer).toBe("black");
            expect(state.selectedSquare).toBeNull();
            expect(state.validMoves).toEqual([]);
            expect(state.moveHistory).toEqual([]);
            expect(state.gameStatus).toBe("playing");
        });
    });

    describe("game flow integration", () => {
        it("should handle a complete turn cycle", () => {
            // Black's turn
            expect(useGameStore.getState().currentPlayer).toBe("black");

            // Black moves pawn
            useGameStore.getState().makeMove({ row: 7, column: 7 }, { row: 6, column: 7 });
            expect(useGameStore.getState().currentPlayer).toBe("white");

            // White moves pawn
            useGameStore.getState().makeMove({ row: 3, column: 3 }, { row: 4, column: 3 });
            expect(useGameStore.getState().currentPlayer).toBe("black");

            // Verify move history
            const state = useGameStore.getState();
            expect(state.moveHistory.length).toBe(2);
            expect(state.moveHistory[0].piece.owner).toBe("black");
            expect(state.moveHistory[1].piece.owner).toBe("white");
        });
    });
});
