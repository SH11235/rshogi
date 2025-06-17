import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import App from "../../App";

describe("Game Flow Integration", () => {
    let user: ReturnType<typeof userEvent.setup>;

    beforeEach(() => {
        user = userEvent.setup();
    });

    describe("Basic Game Flow", () => {
        it("should allow a complete move sequence", async () => {
            render(<App />);

            // Verify initial game state
            expect(screen.getAllByText("先手番")[0]).toBeInTheDocument();
            expect(screen.getAllByText("第1手")[0]).toBeInTheDocument();

            // Find and click a black pawn (7-7 square)
            const squares = screen.getAllByRole("button");
            const pawnSquare = squares.find((square) =>
                square.getAttribute("aria-label")?.includes("Square 7七"),
            );
            expect(pawnSquare).toBeDefined();

            if (pawnSquare) {
                await user.click(pawnSquare);

                // Verify square is selected (blue highlight)
                expect(pawnSquare).toHaveClass("bg-blue-200");

                // Look for valid move indicators (green highlights)
                const validMoveSquares = squares.filter((square) =>
                    square.className.includes("bg-green-100"),
                );
                expect(validMoveSquares.length).toBeGreaterThan(0);

                // Click on a valid move square (7-6)
                const targetSquare = squares.find((square) =>
                    square.getAttribute("aria-label")?.includes("Square 7六"),
                );
                expect(targetSquare).toBeDefined();

                if (targetSquare) {
                    await user.click(targetSquare);

                    // Verify move was made
                    await waitFor(() => {
                        expect(screen.getAllByText("後手番")[0]).toBeInTheDocument();
                        expect(screen.getAllByText("第1手")[0]).toBeInTheDocument();
                    });

                    // Verify move appears in history
                    expect(screen.getAllByText(/☗1\. 歩7六/)[0]).toBeInTheDocument();
                }
            }
        });

        it.skip("should prevent invalid moves", async () => {
            render(<App />);

            const squares = screen.getAllByRole("button");

            // Try to select opponent's piece (white piece)
            const opponentPiece = squares.find((square) =>
                square.getAttribute("aria-label")?.includes("Square 7三"),
            );

            if (opponentPiece) {
                await user.click(opponentPiece);

                // Wait for state to settle
                await waitFor(() => {
                    // Should not be selected (no blue highlight)
                    expect(opponentPiece).not.toHaveClass("bg-blue-200");
                });

                // Turn should still be black's turn
                expect(screen.getAllByText("先手番")[0]).toBeInTheDocument();
            }
        });

        it("should handle game reset", async () => {
            render(<App />);

            // Make a move first
            const squares = screen.getAllByRole("button");
            const pawnSquare = squares.find((square) =>
                square.getAttribute("aria-label")?.includes("Square 7七"),
            );

            if (pawnSquare) {
                await user.click(pawnSquare);
                const targetSquare = squares.find((square) =>
                    square.getAttribute("aria-label")?.includes("Square 7六"),
                );
                if (targetSquare) {
                    await user.click(targetSquare);
                }
            }

            // Wait for move to complete
            await waitFor(() => {
                expect(screen.getAllByText("後手番")[0]).toBeInTheDocument();
            });

            // Click reset button
            const resetButton = screen.getAllByText("リセット")[0];
            await user.click(resetButton);

            // Should show confirmation dialog
            await waitFor(() => {
                expect(screen.getByText("ゲームをリセットしますか？")).toBeInTheDocument();
            });

            // Confirm reset
            const confirmButton = screen.getByText("リセットする");
            await user.click(confirmButton);

            // Verify game is reset
            await waitFor(() => {
                expect(screen.getAllByText("先手番")[0]).toBeInTheDocument();
                expect(screen.getAllByText("第1手")[0]).toBeInTheDocument();
                expect(screen.queryByText(/☗1\. 歩7六/)).not.toBeInTheDocument();
            });
        });
    });

    describe("Undo/Redo Integration", () => {
        it("should allow undoing and redoing moves", async () => {
            render(<App />);

            // Make a move
            const squares = screen.getAllByRole("button");
            const pawnSquare = squares.find((square) =>
                square.getAttribute("aria-label")?.includes("Square 7七"),
            );

            if (pawnSquare) {
                await user.click(pawnSquare);
                const targetSquare = squares.find((square) =>
                    square.getAttribute("aria-label")?.includes("Square 7六"),
                );
                if (targetSquare) {
                    await user.click(targetSquare);
                }
            }

            // Wait for move to complete
            await waitFor(() => {
                expect(screen.getAllByText("後手番")[0]).toBeInTheDocument();
            });

            // Click undo
            const undoButton = screen.getByText("← 戻る");
            expect(undoButton).not.toBeDisabled();
            await user.click(undoButton);

            // Verify undo worked
            await waitFor(() => {
                expect(screen.getAllByText("先手番")[0]).toBeInTheDocument();
                expect(screen.getAllByText("第1手")[0]).toBeInTheDocument();
            });

            // Click redo
            const redoButton = screen.getByText("進む →");
            expect(redoButton).not.toBeDisabled();
            await user.click(redoButton);

            // Verify redo worked
            await waitFor(() => {
                expect(screen.getAllByText("後手番")[0]).toBeInTheDocument();
                expect(screen.getAllByText("第1手")[0]).toBeInTheDocument();
            });
        });

        it("should allow clicking on move history to navigate", async () => {
            render(<App />);

            // Make two moves
            const squares = screen.getAllByRole("button");

            // Black's first move
            const blackPawn = squares.find((square) =>
                square.getAttribute("aria-label")?.includes("Square 7七"),
            );
            if (blackPawn) {
                await user.click(blackPawn);
                const targetSquare = squares.find((square) =>
                    square.getAttribute("aria-label")?.includes("Square 7六"),
                );
                if (targetSquare) {
                    await user.click(targetSquare);
                }
            }

            await waitFor(() => {
                expect(screen.getAllByText("後手番")[0]).toBeInTheDocument();
            });

            // White's first move
            const whitePawn = squares.find((square) =>
                square.getAttribute("aria-label")?.includes("Square 3三"),
            );
            if (whitePawn) {
                await user.click(whitePawn);
                const whiteTarget = squares.find((square) =>
                    square.getAttribute("aria-label")?.includes("Square 3四"),
                );
                if (whiteTarget) {
                    await user.click(whiteTarget);
                }
            }

            await waitFor(() => {
                expect(screen.getAllByText("先手番")[0]).toBeInTheDocument();
                expect(screen.getAllByText("第3手")[0]).toBeInTheDocument();
            });

            // Click on first move in history
            const firstMoveButton = screen.getAllByText(/☗1\. 歩7六/)[0];
            await user.click(firstMoveButton);

            // Should navigate to after first move
            await waitFor(() => {
                expect(screen.getAllByText("後手番")[0]).toBeInTheDocument();
                expect(screen.getAllByText("第1手")[0]).toBeInTheDocument();
            });

            // Click on initial position
            const initialButton = screen.getByText("開始局面");
            await user.click(initialButton);

            // Should navigate to start
            await waitFor(() => {
                expect(screen.getAllByText("先手番")[0]).toBeInTheDocument();
                expect(screen.getAllByText("第1手")[0]).toBeInTheDocument();
            });
        });
    });

    describe("Responsive Layout Integration", () => {
        it("should render mobile layout elements", () => {
            // Set mobile viewport
            Object.defineProperty(window, "innerWidth", {
                writable: true,
                configurable: true,
                value: 400,
            });

            render(<App />);

            // Mobile layout should be rendered (elements exist but might be hidden)
            expect(screen.getByText("将棋")).toBeInTheDocument();
            expect(screen.getAllByText("先手番")[0]).toBeInTheDocument();

            // Board and components should be present
            const squares = screen.getAllByRole("button");
            expect(squares.length).toBe(81); // 9x9 board

            // Move history should be present
            expect(screen.getByText("棋譜")).toBeInTheDocument();
            expect(screen.getByText("← 戻る")).toBeInTheDocument();
            expect(screen.getByText("進む →")).toBeInTheDocument();
        });
    });

    describe("Game State Persistence Through Navigation", () => {
        it("should maintain game state when navigating through history", async () => {
            render(<App />);

            // Make several moves to create a complex state
            const squares = screen.getAllByRole("button");

            // Move 1: 7-7 to 7-6
            const move1Source = squares.find((square) =>
                square.getAttribute("aria-label")?.includes("Square 7七"),
            );
            if (move1Source) {
                await user.click(move1Source);
                const move1Target = squares.find((square) =>
                    square.getAttribute("aria-label")?.includes("Square 7六"),
                );
                if (move1Target) {
                    await user.click(move1Target);
                }
            }

            await waitFor(() => {
                expect(screen.getAllByText("後手番")[0]).toBeInTheDocument();
            });

            // Navigate back to start
            const initialButton = screen.getByText("開始局面");
            await user.click(initialButton);

            await waitFor(() => {
                expect(screen.getAllByText("先手番")[0]).toBeInTheDocument();
                expect(screen.getAllByText("第1手")[0]).toBeInTheDocument();
            });

            // Navigate forward to the move again
            const firstMoveButton = screen.getAllByText(/☗1\. 歩7六/)[0];
            await user.click(firstMoveButton);

            await waitFor(() => {
                expect(screen.getAllByText("後手番")[0]).toBeInTheDocument();
                expect(screen.getAllByText("第1手")[0]).toBeInTheDocument();
            });

            // Verify the board state is correct - the pawn should be on 7-6
            const squareWith76 = squares.find((square) =>
                square.getAttribute("aria-label")?.includes("Square 7六"),
            );
            expect(squareWith76?.textContent).toBe("歩");
        });
    });
});
