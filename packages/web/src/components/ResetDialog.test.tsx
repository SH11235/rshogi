import { fireEvent, render, screen } from "@testing-library/react";
import type { Move } from "shogi-core";
import { GameInfo } from "./GameInfo";

describe("GameInfo Reset Functionality", () => {
    const mockReset = vi.fn();

    beforeEach(() => {
        mockReset.mockClear();
    });

    it("should reset immediately when no moves have been played", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="playing"
                moveHistory={[]}
                onReset={mockReset}
            />,
        );

        const resetButton = screen.getByText("リセット");
        fireEvent.click(resetButton);

        expect(mockReset).toHaveBeenCalledTimes(1);
    });

    it("should reset immediately when game is over", () => {
        const moveHistory: Move[] = [
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
        ];

        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="checkmate"
                moveHistory={moveHistory}
                onReset={mockReset}
            />,
        );

        const newGameButton = screen.getByText("新しいゲーム");
        fireEvent.click(newGameButton);

        expect(mockReset).toHaveBeenCalledTimes(1);
    });

    it("should show confirmation dialog when resetting game in progress", () => {
        const moveHistory: Move[] = [
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
        ];

        render(
            <GameInfo
                currentPlayer="white"
                gameStatus="playing"
                moveHistory={moveHistory}
                onReset={mockReset}
            />,
        );

        const resetButton = screen.getByText("リセット");
        fireEvent.click(resetButton);

        // Confirmation dialog should appear
        expect(screen.getByText("ゲームをリセットしますか？")).toBeInTheDocument();
        expect(
            screen.getByText(
                "現在の対局がリセットされ、すべての手順が失われます。この操作は元に戻せません。",
            ),
        ).toBeInTheDocument();

        // Reset should not be called yet
        expect(mockReset).not.toHaveBeenCalled();
    });

    it("should reset when confirming in dialog", () => {
        const moveHistory: Move[] = [
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
        ];

        render(
            <GameInfo
                currentPlayer="white"
                gameStatus="playing"
                moveHistory={moveHistory}
                onReset={mockReset}
            />,
        );

        const resetButton = screen.getByText("リセット");
        fireEvent.click(resetButton);

        const confirmButton = screen.getByText("リセットする");
        fireEvent.click(confirmButton);

        expect(mockReset).toHaveBeenCalledTimes(1);
    });

    it("should not reset when canceling in dialog", () => {
        const moveHistory: Move[] = [
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
        ];

        render(
            <GameInfo
                currentPlayer="white"
                gameStatus="playing"
                moveHistory={moveHistory}
                onReset={mockReset}
            />,
        );

        const resetButton = screen.getByText("リセット");
        fireEvent.click(resetButton);

        const cancelButton = screen.getByText("キャンセル");
        fireEvent.click(cancelButton);

        expect(mockReset).not.toHaveBeenCalled();
    });

    it("should show correct button text based on game status", () => {
        const { rerender } = render(
            <GameInfo
                currentPlayer="black"
                gameStatus="playing"
                moveHistory={[]}
                onReset={mockReset}
            />,
        );

        expect(screen.getByText("リセット")).toBeInTheDocument();

        rerender(
            <GameInfo
                currentPlayer="black"
                gameStatus="checkmate"
                moveHistory={[]}
                onReset={mockReset}
            />,
        );

        expect(screen.getByText("新しいゲーム")).toBeInTheDocument();
    });

    it("should display move count correctly", () => {
        const moveHistory: Move[] = [
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
            {
                type: "drop",
                to: { row: 5, column: 5 },
                piece: { type: "pawn", owner: "white", promoted: false },
            },
        ];

        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="playing"
                moveHistory={moveHistory}
                onReset={mockReset}
            />,
        );

        expect(screen.getByText("第2手")).toBeInTheDocument();
        expect(screen.getByText("総手数: 2")).toBeInTheDocument();
    });
});
