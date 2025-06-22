import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HISTORY_CURSOR } from "shogi-core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";

// Mock the game store with minimal implementation
const mockSelectSquare = vi.fn();
const mockResetGame = vi.fn();
const mockImportGame = vi.fn();

const mockState = {
    board: {}, // Simple mock board
    hands: { black: {}, white: {} },
    currentPlayer: "black",
    selectedSquare: null,
    selectedDropPiece: null,
    validMoves: [],
    validDropSquares: [],
    gameStatus: "playing",
    moveHistory: [],
    historyCursor: HISTORY_CURSOR.LATEST_POSITION,
    promotionPending: null,
    resignedPlayer: null,
    branchInfo: null,
    isTsumeShogi: false,
    gameMode: "playing",
    reviewBasePosition: null,
    timer: {
        config: {
            mode: null,
            basicTime: 0,
            byoyomiTime: 0,
            fischerIncrement: 0,
            perMoveLimit: 0,
        },
        blackTime: 0,
        whiteTime: 0,
        blackInByoyomi: false,
        whiteInByoyomi: false,
        activePlayer: null,
        isPaused: false,
        lastTickTime: 0,
        blackWarningLevel: "normal",
        whiteWarningLevel: "normal",
        hasTimedOut: false,
        timedOutPlayer: null,
    },
    selectSquare: mockSelectSquare,
    selectDropPiece: vi.fn(),
    confirmPromotion: vi.fn(),
    cancelPromotion: vi.fn(),
    clearSelections: vi.fn(),
    resetGame: mockResetGame,
    importGame: mockImportGame,
    importSfen: vi.fn(),
    resign: vi.fn(),
    undo: vi.fn(),
    redo: vi.fn(),
    goToMove: vi.fn(),
    canUndo: vi.fn(() => false),
    canRedo: vi.fn(() => false),
    navigateNext: vi.fn(),
    navigatePrevious: vi.fn(),
    navigateFirst: vi.fn(),
    navigateLast: vi.fn(),
    returnToMainLine: vi.fn(),
    isInBranch: vi.fn(() => false),
    canNavigateNext: vi.fn(() => false),
    canNavigatePrevious: vi.fn(() => false),
    startGameFromPosition: vi.fn(),
    returnToReviewMode: vi.fn(),
    pauseTimer: vi.fn(),
    resumeTimer: vi.fn(),
    gameUndo: vi.fn(),
    gameRedo: vi.fn(),
    canGameUndo: vi.fn(() => false),
    canGameRedo: vi.fn(() => false),
};

// GameStateの部分型を定義
type PartialGameState = typeof mockState;

vi.mock("./stores/gameStore", () => ({
    useGameStore: <T,>(selector?: (state: PartialGameState) => T) => {
        return selector ? selector(mockState) : mockState;
    },
}));

describe("App component", () => {
    let user: ReturnType<typeof userEvent.setup>;

    beforeEach(() => {
        vi.clearAllMocks();
        user = userEvent.setup();
    });

    it("should render the app without errors", () => {
        const { container } = render(<App />);
        expect(container).toBeTruthy();
    });

    it("should display the main game title", () => {
        render(<App />);
        const titles = screen.getAllByText("将棋");
        expect(titles.length).toBeGreaterThan(0);
    });

    it("should display initial game state", () => {
        render(<App />);
        // Check for initial turn display
        expect(screen.getAllByText("先手番")[0]).toBeInTheDocument();
        // Check for board presence (81 squares)
        const squares = screen.getAllByRole("button");
        expect(squares.length).toBeGreaterThanOrEqual(81); // Board squares + other buttons
    });

    it("should allow clicking on board squares", async () => {
        render(<App />);

        const squares = screen.getAllByRole("button");
        const boardSquare = squares.find((square) =>
            square.getAttribute("aria-label")?.includes("Square"),
        );

        if (boardSquare) {
            await user.click(boardSquare);
            expect(mockSelectSquare).toHaveBeenCalled();
        }
    });

    it("should have a functional reset button", async () => {
        render(<App />);

        const resetButton = screen.getAllByText("リセット")[0];
        await user.click(resetButton);

        expect(mockResetGame).toHaveBeenCalled();
    });

    it("should display captured pieces sections", () => {
        render(<App />);

        expect(screen.getAllByText("先手の持ち駒")[0]).toBeInTheDocument();
        expect(screen.getAllByText("後手の持ち駒")[0]).toBeInTheDocument();
    });

    it("should display move history section", () => {
        render(<App />);

        expect(screen.getAllByText("棋譜")[0]).toBeInTheDocument();
        expect(screen.getAllByText("開始局面")[0]).toBeInTheDocument();
    });
});
