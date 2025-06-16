import type { Meta, StoryObj } from "@storybook/react";
import { GameInfo } from "./GameInfo";

const meta: Meta<typeof GameInfo> = {
    title: "Shogi/GameInfo",
    component: GameInfo,
    parameters: {
        layout: "centered",
    },
    tags: ["autodocs"],
    argTypes: {
        currentPlayer: {
            control: { type: "select" },
            options: ["black", "white"],
        },
        gameStatus: {
            control: { type: "select" },
            options: [
                "playing",
                "check",
                "checkmate",
                "draw",
                "black_win",
                "white_win",
                "resigned",
            ],
        },
    },
};

export default meta;
type Story = StoryObj<typeof meta>;

export const BlackTurn: Story = {
    args: {
        currentPlayer: "black",
        gameStatus: "playing",
        moveHistory: [],
        onReset: () => console.log("ゲームリセット"),
    },
};

export const WhiteTurn: Story = {
    args: {
        currentPlayer: "white",
        gameStatus: "playing",
        moveHistory: [],
        onReset: () => console.log("ゲームリセット"),
    },
};

export const BlackWins: Story = {
    args: {
        currentPlayer: "black",
        gameStatus: "black_win",
        moveHistory: [],
        onReset: () => console.log("ゲームリセット"),
    },
};

export const WhiteWins: Story = {
    args: {
        currentPlayer: "white",
        gameStatus: "white_win",
        moveHistory: [],
        onReset: () => console.log("ゲームリセット"),
    },
};

export const Check: Story = {
    args: {
        currentPlayer: "white",
        gameStatus: "check",
        moveHistory: [],
        onReset: () => console.log("ゲームリセット"),
    },
};

export const Draw: Story = {
    args: {
        currentPlayer: "black",
        gameStatus: "draw",
        moveHistory: [],
        onReset: () => console.log("ゲームリセット"),
    },
};

// Story to test confirmation dialog during ongoing game
export const GameInProgressWithMoves: Story = {
    args: {
        currentPlayer: "white",
        gameStatus: "playing",
        moveHistory: [
            {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 6, column: 7 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
            {
                type: "move",
                from: { row: 3, column: 3 },
                to: { row: 4, column: 3 },
                piece: { type: "pawn", owner: "white", promoted: false },
                promote: false,
                captured: null,
            },
        ],
        onReset: () => console.log("ゲームリセット"),
    },
};
