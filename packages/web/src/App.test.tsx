import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";

// Mock the game store
const mockUndo = vi.fn();
const mockRedo = vi.fn();
const mockCanUndo = vi.fn(() => true);
const mockCanRedo = vi.fn(() => true);

vi.mock("./stores/gameStore", () => ({
    useGameStore: () => ({
        board: {},
        hands: { black: {}, white: {} },
        currentPlayer: "black",
        selectedSquare: null,
        selectedDropPiece: null,
        validMoves: [],
        validDropSquares: [],
        gameStatus: "playing",
        moveHistory: [],
        historyCursor: -1,
        promotionPending: null,
        resignedPlayer: null,
        selectSquare: vi.fn(),
        selectDropPiece: vi.fn(),
        confirmPromotion: vi.fn(),
        cancelPromotion: vi.fn(),
        resetGame: vi.fn(),
        resign: vi.fn(),
        undo: mockUndo,
        redo: mockRedo,
        goToMove: vi.fn(),
        canUndo: mockCanUndo,
        canRedo: mockCanRedo,
    }),
}));

describe("App component", () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    it("should render the app without errors", () => {
        const { container } = render(<App />);
        expect(container).toBeTruthy();
    });

    it("should display the main game title", () => {
        render(<App />);
        expect(screen.getByText("将棋")).toBeInTheDocument();
    });
});
