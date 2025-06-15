import { fireEvent, render, screen } from "@testing-library/react";
import { type Board as BoardType, createPiece, modernInitialBoard } from "shogi-core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { Board } from "./Board";

describe("Board component", () => {
    const mockOnSquareClick = vi.fn();

    // Helper to create empty board that satisfies Board type
    const createEmptyBoard = (): BoardType => {
        const board: Partial<BoardType> = {};
        for (let row = 1; row <= 9; row++) {
            for (let col = 1; col <= 9; col++) {
                board[`${row}${col}` as keyof BoardType] = null;
            }
        }
        return board as BoardType;
    };

    // Helper to create partial board for testing
    const createPartialBoard = (
        pieces: Record<string, ReturnType<typeof createPiece>>,
    ): BoardType => {
        const board = createEmptyBoard();
        for (const [key, piece] of Object.entries(pieces)) {
            (board as Record<string, typeof piece>)[key] = piece;
        }
        return board;
    };

    beforeEach(() => {
        mockOnSquareClick.mockClear();
    });

    it("renders 9x9 grid correctly", () => {
        render(
            <Board
                board={modernInitialBoard}
                selectedSquare={null}
                validMoves={[]}
                onSquareClick={mockOnSquareClick}
            />,
        );

        // Should have 81 squares (9x9)
        const squares = screen.getAllByRole("button");
        expect(squares).toHaveLength(81);
    });

    it("displays pieces correctly", () => {
        render(
            <Board
                board={modernInitialBoard}
                selectedSquare={null}
                validMoves={[]}
                onSquareClick={mockOnSquareClick}
            />,
        );

        // Check for some initial pieces
        expect(screen.getAllByText("歩")).toHaveLength(18); // 9 for each side
        expect(screen.getAllByText("飛")).toHaveLength(2); // 1 for each side
        expect(screen.getAllByText("角")).toHaveLength(2); // 1 for each side
    });

    it("handles square clicks correctly", () => {
        render(
            <Board
                board={modernInitialBoard}
                selectedSquare={null}
                validMoves={[]}
                onSquareClick={mockOnSquareClick}
            />,
        );

        const squares = screen.getAllByRole("button");
        const firstSquare = squares[0];

        fireEvent.click(firstSquare);

        expect(mockOnSquareClick).toHaveBeenCalledTimes(1);
        expect(mockOnSquareClick).toHaveBeenCalledWith({ row: 1, column: 9 });
    });

    it("squares are keyboard accessible", () => {
        render(
            <Board
                board={modernInitialBoard}
                selectedSquare={null}
                validMoves={[]}
                onSquareClick={mockOnSquareClick}
            />,
        );

        const squares = screen.getAllByRole("button");
        const firstSquare = squares[0];

        // Test that the square can receive focus (has tabIndex)
        expect(firstSquare).toBeInTheDocument();
        expect(firstSquare.tagName).toBe("BUTTON");
    });

    it("highlights selected square", () => {
        const selectedSquare = { row: 7 as const, column: 7 as const };

        render(
            <Board
                board={modernInitialBoard}
                selectedSquare={selectedSquare}
                validMoves={[]}
                onSquareClick={mockOnSquareClick}
            />,
        );

        // Find the selected square and check its styling
        const squares = screen.getAllByRole("button");
        const selectedSquareElement = squares.find((square) =>
            square.className.includes("bg-blue-200"),
        );

        expect(selectedSquareElement).toBeDefined();
    });

    it("highlights valid moves", () => {
        const validMoves = [
            { row: 6 as const, column: 7 as const },
            { row: 5 as const, column: 7 as const },
        ];

        render(
            <Board
                board={modernInitialBoard}
                selectedSquare={{ row: 7 as const, column: 7 as const }}
                validMoves={validMoves}
                onSquareClick={mockOnSquareClick}
            />,
        );

        // Should have squares with valid move highlighting
        const squares = screen.getAllByRole("button");
        const validMoveSquares = squares.filter((square) =>
            square.className.includes("bg-green-100"),
        );

        expect(validMoveSquares).toHaveLength(2);
    });

    it("renders empty board correctly", () => {
        const emptyBoard = createEmptyBoard();
        render(
            <Board
                board={emptyBoard}
                selectedSquare={null}
                validMoves={[]}
                onSquareClick={mockOnSquareClick}
            />,
        );

        // Should still have 81 squares
        const squares = screen.getAllByRole("button");
        expect(squares).toHaveLength(81);

        // Should not have any pieces
        expect(screen.queryByText("歩")).not.toBeInTheDocument();
        expect(screen.queryByText("飛")).not.toBeInTheDocument();
    });

    it("renders column numbers correctly", () => {
        render(
            <Board
                board={modernInitialBoard}
                selectedSquare={null}
                validMoves={[]}
                onSquareClick={mockOnSquareClick}
            />,
        );

        // Should show column numbers 1-9
        for (let i = 1; i <= 9; i++) {
            expect(screen.getByText(i.toString())).toBeInTheDocument();
        }
    });

    it("handles custom board state", () => {
        const customBoard = createPartialBoard({
            "55": createPiece("king", "black"),
            "51": createPiece("rook", "white"),
        });

        render(
            <Board
                board={customBoard}
                selectedSquare={null}
                validMoves={[]}
                onSquareClick={mockOnSquareClick}
            />,
        );

        expect(screen.getByText("王")).toBeInTheDocument();
        expect(screen.getByText("飛")).toBeInTheDocument();

        // Should only have these two pieces
        const allPieceTexts = screen.getAllByText(/[歩香桂銀金角飛王玉]/);
        expect(allPieceTexts).toHaveLength(2);
    });

    it("calculates square coordinates correctly", () => {
        render(
            <Board
                board={modernInitialBoard}
                selectedSquare={null}
                validMoves={[]}
                onSquareClick={mockOnSquareClick}
            />,
        );

        const squares = screen.getAllByRole("button");

        // Click the last square (bottom-right)
        const lastSquare = squares[squares.length - 1];
        fireEvent.click(lastSquare);

        expect(mockOnSquareClick).toHaveBeenCalledWith({ row: 9, column: 1 });
    });
});
