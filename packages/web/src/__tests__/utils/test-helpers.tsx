import { type RenderOptions, type Screen, render } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import type { ReactElement } from "react";
import { type Board as BoardType, createPiece, modernInitialBoard } from "shogi-core";
import { expect } from "vitest";

// Custom render function for integration tests
export function renderWithGameContext(ui: ReactElement, options?: RenderOptions) {
    return render(ui, options);
}

// Helper to create custom board states for testing
export function createTestBoard(pieces: Record<string, ReturnType<typeof createPiece>>): BoardType {
    const board: Partial<BoardType> = {};

    // Initialize empty board
    for (let row = 1; row <= 9; row++) {
        for (let col = 1; col <= 9; col++) {
            board[`${row}${col}` as keyof BoardType] = null;
        }
    }

    // Add specified pieces
    for (const [key, piece] of Object.entries(pieces)) {
        (board as Record<string, typeof piece>)[key] = piece;
    }

    return board as BoardType;
}

// Helper to create board states for testing specific scenarios
export const createScenarioBoard = {
    // Empty board
    empty: (): BoardType => createTestBoard({}),

    // Board with pieces near promotion zones
    nearPromotion: (): BoardType =>
        createTestBoard({
            "23": createPiece("pawn", "black"), // Black pawn near white promotion zone
            "73": createPiece("pawn", "white"), // White pawn near black promotion zone
            "51": createPiece("king", "white"), // White king
            "59": createPiece("king", "black"), // Black king
        }),

    // Board with capturable pieces
    withCaptures: (): BoardType =>
        createTestBoard({
            "55": createPiece("king", "black"),
            "51": createPiece("king", "white"),
            "54": createPiece("pawn", "black"),
            "56": createPiece("pawn", "white"),
            "44": createPiece("silver", "black"),
            "66": createPiece("silver", "white"),
        }),

    // Board in check
    inCheck: (): BoardType =>
        createTestBoard({
            "51": createPiece("king", "white"),
            "52": createPiece("rook", "black"),
            "59": createPiece("king", "black"),
        }),

    // Board near checkmate
    nearCheckmate: (): BoardType =>
        createTestBoard({
            "11": createPiece("king", "white"),
            "21": createPiece("gold", "black"),
            "12": createPiece("gold", "black"),
            "59": createPiece("king", "black"),
        }),
};

// Helper function to simulate game moves in tests
export async function simulateGameMoves(
    moves: Array<{ from: string; to: string }>,
    userEvent: UserEvent,
    screen: Screen,
) {
    for (const move of moves) {
        const squares = screen.getAllByRole("button");

        const sourceSquare = squares.find((square: HTMLElement) =>
            square.getAttribute("aria-label")?.includes(move.from),
        );

        if (sourceSquare) {
            await userEvent.click(sourceSquare);

            const targetSquare = squares.find((square: HTMLElement) =>
                square.getAttribute("aria-label")?.includes(move.to),
            );

            if (targetSquare) {
                await userEvent.click(targetSquare);
            }
        }
    }
}

// Helper to wait for specific game states
export async function waitForGameState(
    screen: Screen,
    waitFor: typeof import("@testing-library/react").waitFor,
    expectedState: {
        player?: "先手" | "後手";
        move?: number;
        status?: string;
    },
) {
    if (expectedState.player) {
        await waitFor(() => {
            expect(screen.getByText(`${expectedState.player}の番です`)).toBeInTheDocument();
        });
    }

    if (expectedState.move) {
        await waitFor(() => {
            expect(screen.getByText(`第${expectedState.move}手`)).toBeInTheDocument();
        });
    }

    if (expectedState.status) {
        const status = expectedState.status;
        await waitFor(() => {
            expect(screen.getByText(status)).toBeInTheDocument();
        });
    }
}

// Helper to check if a square contains a specific piece
export function expectSquareToHavePiece(screen: Screen, squareLabel: string, pieceType: string) {
    const squares = screen.getAllByRole("button");
    const square = squares.find((square: HTMLElement) =>
        square.getAttribute("aria-label")?.includes(squareLabel),
    );

    expect(square).toBeDefined();
    if (square) {
        expect(square.textContent).toBe(pieceType);
    }
}

// Helper to verify move history contains specific moves
export function expectMoveHistoryToContain(screen: Screen, moveNotation: string) {
    expect(screen.getByText(new RegExp(moveNotation))).toBeInTheDocument();
}

// Helper to get all valid move indicators on the board
export function getValidMoveSquares(screen: Screen) {
    const squares = screen.getAllByRole("button");
    return squares.filter((square: HTMLElement) => square.className.includes("bg-green-100"));
}

// Helper to get all valid drop indicators on the board
export function getValidDropSquares(screen: Screen) {
    const squares = screen.getAllByRole("button");
    return squares.filter((square: HTMLElement) => square.className.includes("bg-purple-100"));
}

// Helper to check if undo/redo buttons are in expected states
export function expectUndoRedoState(screen: Screen, undoEnabled: boolean, redoEnabled: boolean) {
    const undoButton = screen.getByText("← 戻る");
    const redoButton = screen.getByText("進む →");

    if (undoEnabled) {
        expect(undoButton).not.toBeDisabled();
    } else {
        expect(undoButton).toBeDisabled();
    }

    if (redoEnabled) {
        expect(redoButton).not.toBeDisabled();
    } else {
        expect(redoButton).toBeDisabled();
    }
}

// Helper to simulate complex game scenarios
export const gameScenarios = {
    // Quick game with a few moves
    quickGame: [
        { from: "Square 7七", to: "Square 7六" },
        { from: "Square 3三", to: "Square 3四" },
        { from: "Square 2七", to: "Square 2六" },
        { from: "Square 4三", to: "Square 4四" },
    ],

    // Game focused on center control
    centerControl: [
        { from: "Square 5七", to: "Square 5六" },
        { from: "Square 5三", to: "Square 5四" },
        { from: "Square 5六", to: "Square 5五" },
        { from: "Square 5四", to: "Square 5五" }, // Capture
    ],

    // Game with piece development
    development: [
        { from: "Square 7七", to: "Square 7六" },
        { from: "Square 3三", to: "Square 3四" },
        { from: "Square 6八", to: "Square 7七" }, // Silver up
        { from: "Square 4二", to: "Square 3三" }, // Silver up
    ],
};

// Mock viewport helper for responsive testing
export function mockViewport(width: number, height = 800) {
    Object.defineProperty(window, "innerWidth", {
        writable: true,
        configurable: true,
        value: width,
    });

    Object.defineProperty(window, "innerHeight", {
        writable: true,
        configurable: true,
        value: height,
    });

    // Trigger resize event
    window.dispatchEvent(new Event("resize"));
}

// Helper for testing touch interactions
export function mockTouchDevice() {
    Object.defineProperty(navigator, "maxTouchPoints", {
        writable: true,
        configurable: true,
        value: 5,
    });

    // Add touch event support for testing
    Object.defineProperty(window, "TouchEvent", {
        writable: true,
        configurable: true,
        value: class extends Event {
            constructor(type: string, options: TouchEventInit = {}) {
                super(type, options);
            }
        },
    });
}

export { modernInitialBoard };
