import { fireEvent, render, screen } from "@testing-library/react";
import type { Move } from "shogi-core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { GameInfo } from "./GameInfo";

describe("GameInfo component", () => {
    const mockOnReset = vi.fn();
    const mockMoveHistory: Move[] = []; // Empty move history for basic tests

    beforeEach(() => {
        mockOnReset.mockClear();
    });

    it("displays black turn correctly", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="playing"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        expect(screen.getByText("先手番")).toBeInTheDocument();
        expect(screen.getByText("リセット")).toBeInTheDocument();
        expect(screen.getByText("第1手")).toBeInTheDocument();
        expect(screen.getByText("総手数: 0")).toBeInTheDocument();
    });

    it("displays white turn correctly", () => {
        render(
            <GameInfo
                currentPlayer="white"
                gameStatus="playing"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        expect(screen.getByText("後手番")).toBeInTheDocument();
        expect(screen.getByText("リセット")).toBeInTheDocument();
    });

    it("displays black win correctly", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="black_win"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        expect(screen.getByText("先手の勝ち！")).toBeInTheDocument();
        expect(screen.getByText("新しいゲーム")).toBeInTheDocument();
    });

    it("displays white win correctly", () => {
        render(
            <GameInfo
                currentPlayer="white"
                gameStatus="white_win"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        expect(screen.getByText("後手の勝ち！")).toBeInTheDocument();
        expect(screen.getByText("新しいゲーム")).toBeInTheDocument();
    });

    it("displays draw correctly", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="draw"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        expect(screen.getByText("引き分け")).toBeInTheDocument();
        expect(screen.getByText("新しいゲーム")).toBeInTheDocument();
    });

    it("displays check status correctly", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="check"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        expect(screen.getByText("王手！ - 先手番")).toBeInTheDocument();
        expect(screen.getByText("🔥 王手がかかっています")).toBeInTheDocument();
    });

    it("displays checkmate status correctly", () => {
        render(
            <GameInfo
                currentPlayer="white"
                gameStatus="checkmate"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        expect(screen.getByText("詰み")).toBeInTheDocument();
        expect(screen.getByText("ゲーム終了")).toBeInTheDocument();
    });

    it("calls onReset when button is clicked", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="playing"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        const resetButton = screen.getByText("リセット");
        fireEvent.click(resetButton);

        expect(mockOnReset).toHaveBeenCalledTimes(1);
    });

    it("applies correct styling for black player", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="playing"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        const title = screen.getByText("先手番");
        expect(title).toHaveClass("text-black");
    });

    it("applies correct styling for white player", () => {
        render(
            <GameInfo
                currentPlayer="white"
                gameStatus="playing"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        const title = screen.getByText("後手番");
        expect(title).toHaveClass("text-red-600");
    });

    it("applies correct styling for check status", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="check"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        const title = screen.getByText("王手！ - 先手番");
        expect(title).toHaveClass("text-red-600");
    });

    it("has correct button type", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="playing"
                moveHistory={mockMoveHistory}
                onReset={mockOnReset}
            />,
        );

        const button = screen.getByRole("button");
        expect(button).toHaveAttribute("type", "button");
    });
});
