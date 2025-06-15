import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { GameInfo } from "./GameInfo";

describe("GameInfo component", () => {
    const mockOnReset = vi.fn();

    beforeEach(() => {
        mockOnReset.mockClear();
    });

    it("displays black turn correctly", () => {
        render(<GameInfo currentPlayer="black" gameStatus="playing" onReset={mockOnReset} />);

        expect(screen.getByText("先手番")).toBeInTheDocument();
        expect(screen.getByText("リセット")).toBeInTheDocument();
    });

    it("displays white turn correctly", () => {
        render(<GameInfo currentPlayer="white" gameStatus="playing" onReset={mockOnReset} />);

        expect(screen.getByText("後手番")).toBeInTheDocument();
        expect(screen.getByText("リセット")).toBeInTheDocument();
    });

    it("displays black win correctly", () => {
        render(<GameInfo currentPlayer="black" gameStatus="black_win" onReset={mockOnReset} />);

        expect(screen.getByText("先手の勝ち！")).toBeInTheDocument();
        expect(screen.getByText("もう一度")).toBeInTheDocument();
    });

    it("displays white win correctly", () => {
        render(<GameInfo currentPlayer="white" gameStatus="white_win" onReset={mockOnReset} />);

        expect(screen.getByText("後手の勝ち！")).toBeInTheDocument();
        expect(screen.getByText("もう一度")).toBeInTheDocument();
    });

    it("displays draw correctly", () => {
        render(<GameInfo currentPlayer="black" gameStatus="draw" onReset={mockOnReset} />);

        expect(screen.getByText("引き分け")).toBeInTheDocument();
        expect(screen.getByText("もう一度")).toBeInTheDocument();
    });

    it("calls onReset when button is clicked", () => {
        render(<GameInfo currentPlayer="black" gameStatus="playing" onReset={mockOnReset} />);

        const resetButton = screen.getByText("リセット");
        fireEvent.click(resetButton);

        expect(mockOnReset).toHaveBeenCalledTimes(1);
    });

    it("applies correct styling for black player", () => {
        render(<GameInfo currentPlayer="black" gameStatus="playing" onReset={mockOnReset} />);

        const title = screen.getByText("先手番");
        expect(title).toHaveClass("text-black");
        expect(title).not.toHaveClass("text-red-600");
    });

    it("applies correct styling for white player", () => {
        render(<GameInfo currentPlayer="white" gameStatus="playing" onReset={mockOnReset} />);

        const title = screen.getByText("後手番");
        expect(title).toHaveClass("text-red-600");
        expect(title).not.toHaveClass("text-black");
    });

    it("applies gray styling when game is over", () => {
        render(<GameInfo currentPlayer="black" gameStatus="black_win" onReset={mockOnReset} />);

        const title = screen.getByText("先手の勝ち！");
        expect(title).toHaveClass("text-gray-600");
    });

    it("has correct button type", () => {
        render(<GameInfo currentPlayer="black" gameStatus="playing" onReset={mockOnReset} />);

        const button = screen.getByRole("button");
        expect(button).toHaveAttribute("type", "button");
    });
});
