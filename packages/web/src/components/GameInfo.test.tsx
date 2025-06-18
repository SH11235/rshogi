import { fireEvent, render, screen } from "@testing-library/react";
import type { Move } from "shogi-core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { GameInfo } from "./GameInfo";

describe("GameInfo component", () => {
    const mockOnReset = vi.fn();
    const mockOnResign = vi.fn();
    const mockMoveHistory: Move[] = []; // Empty move history for basic tests

    beforeEach(() => {
        mockOnReset.mockClear();
        mockOnResign.mockClear();
    });

    it("displays black turn correctly", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="playing"
                moveHistory={mockMoveHistory}
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
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
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
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
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
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
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
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
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
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
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
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
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
            />,
        );

        expect(screen.getByText("詰み")).toBeInTheDocument();
        expect(screen.getByText("🏁 ゲーム終了")).toBeInTheDocument();
    });

    it("calls onReset when button is clicked", () => {
        render(
            <GameInfo
                currentPlayer="black"
                gameStatus="playing"
                moveHistory={mockMoveHistory}
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
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
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
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
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
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
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
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
                historyCursor={-1}
                resignedPlayer={null}
                onReset={mockOnReset}
                onResign={mockOnResign}
            />,
        );

        const buttons = screen.getAllByRole("button");
        // Check that all buttons have correct type attribute
        for (const button of buttons) {
            expect(button).toHaveAttribute("type", "button");
        }
    });

    describe("Resignation functionality", () => {
        it("shows resign button during game with moves", () => {
            const moveHistoryWithMoves: Move[] = [
                {
                    type: "move",
                    from: { row: 7, column: 5 },
                    to: { row: 6, column: 5 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
            ];

            render(
                <GameInfo
                    currentPlayer="black"
                    gameStatus="playing"
                    moveHistory={moveHistoryWithMoves}
                    resignedPlayer={null}
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            expect(screen.getByText("投了")).toBeInTheDocument();
        });

        it("does not show resign button at game start", () => {
            render(
                <GameInfo
                    currentPlayer="black"
                    gameStatus="playing"
                    moveHistory={[]}
                    resignedPlayer={null}
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            expect(screen.queryByText("投了")).not.toBeInTheDocument();
        });

        it("does not show resign button when game is over", () => {
            const moveHistoryWithMoves: Move[] = [
                {
                    type: "move",
                    from: { row: 7, column: 5 },
                    to: { row: 6, column: 5 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
            ];

            render(
                <GameInfo
                    currentPlayer="black"
                    gameStatus="black_win"
                    moveHistory={moveHistoryWithMoves}
                    resignedPlayer={null}
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            expect(screen.queryByText("投了")).not.toBeInTheDocument();
        });

        it("calls onResign when resign button is clicked and confirmed", async () => {
            const moveHistoryWithMoves: Move[] = [
                {
                    type: "move",
                    from: { row: 7, column: 5 },
                    to: { row: 6, column: 5 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
            ];

            render(
                <GameInfo
                    currentPlayer="black"
                    gameStatus="playing"
                    moveHistory={moveHistoryWithMoves}
                    resignedPlayer={null}
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            // Click resign button
            const resignButton = screen.getByText("投了");
            fireEvent.click(resignButton);

            // Confirm resignation
            const confirmButton = screen.getByText("投了する");
            fireEvent.click(confirmButton);

            expect(mockOnResign).toHaveBeenCalledTimes(1);
        });
    });

    describe("Detailed game end display", () => {
        it("displays checkmate victory details for black win", () => {
            const moveHistoryWithMoves: Move[] = [
                {
                    type: "move",
                    from: { row: 7, column: 5 },
                    to: { row: 6, column: 5 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
            ];

            render(
                <GameInfo
                    currentPlayer="white"
                    gameStatus="black_win"
                    moveHistory={moveHistoryWithMoves}
                    resignedPlayer={null}
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            expect(screen.getByText("先手の勝ち！")).toBeInTheDocument();
            expect(screen.getByText("詰みにより勝利")).toBeInTheDocument();
            expect(screen.getByText("第2手までで決着")).toBeInTheDocument();
        });

        it("displays resignation victory details for black win", () => {
            const moveHistoryWithMoves: Move[] = [
                {
                    type: "move",
                    from: { row: 7, column: 5 },
                    to: { row: 6, column: 5 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
            ];

            render(
                <GameInfo
                    currentPlayer="white"
                    gameStatus="black_win"
                    moveHistory={moveHistoryWithMoves}
                    resignedPlayer="white"
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            expect(screen.getByText("先手の勝ち！")).toBeInTheDocument();
            expect(screen.getByText("後手が投了しました")).toBeInTheDocument();
            expect(screen.getByText("第2手までで決着")).toBeInTheDocument();
        });

        it("displays checkmate victory details for white win", () => {
            const moveHistoryWithMoves: Move[] = [
                {
                    type: "move",
                    from: { row: 7, column: 5 },
                    to: { row: 6, column: 5 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                {
                    type: "move",
                    from: { row: 3, column: 5 },
                    to: { row: 4, column: 5 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
            ];

            render(
                <GameInfo
                    currentPlayer="black"
                    gameStatus="white_win"
                    moveHistory={moveHistoryWithMoves}
                    resignedPlayer={null}
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            expect(screen.getByText("後手の勝ち！")).toBeInTheDocument();
            expect(screen.getByText("詰みにより勝利")).toBeInTheDocument();
            expect(screen.getByText("第3手までで決着")).toBeInTheDocument();
        });

        it("displays resignation victory details for white win", () => {
            const moveHistoryWithMoves: Move[] = [
                {
                    type: "move",
                    from: { row: 7, column: 5 },
                    to: { row: 6, column: 5 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                {
                    type: "move",
                    from: { row: 3, column: 5 },
                    to: { row: 4, column: 5 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
            ];

            render(
                <GameInfo
                    currentPlayer="black"
                    gameStatus="white_win"
                    moveHistory={moveHistoryWithMoves}
                    resignedPlayer="black"
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            expect(screen.getByText("後手の勝ち！")).toBeInTheDocument();
            expect(screen.getByText("先手が投了しました")).toBeInTheDocument();
            expect(screen.getByText("第3手までで決着")).toBeInTheDocument();
        });

        it("displays other game end statuses correctly", () => {
            render(
                <GameInfo
                    currentPlayer="black"
                    gameStatus="sennichite"
                    moveHistory={[]}
                    resignedPlayer={null}
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            expect(screen.getByText("千日手")).toBeInTheDocument();
            expect(screen.getByText("同一局面が4回現れました")).toBeInTheDocument();
        });

        it("displays timeout status correctly", () => {
            render(
                <GameInfo
                    currentPlayer="black"
                    gameStatus="timeout"
                    moveHistory={[]}
                    resignedPlayer={null}
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            expect(screen.getByText("時間切れ")).toBeInTheDocument();
            expect(screen.getByText("持ち時間が切れました")).toBeInTheDocument();
        });

        it("applies correct styling for victory status", () => {
            render(
                <GameInfo
                    currentPlayer="black"
                    gameStatus="black_win"
                    moveHistory={[]}
                    resignedPlayer={null}
                    onReset={mockOnReset}
                    onResign={mockOnResign}
                />,
            );

            const title = screen.getByText("先手の勝ち！");
            expect(title).toHaveClass("text-green-600");
        });
    });
});
