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
        onReset: () => console.log("ゲームリセット"),
    },
};

export const WhiteTurn: Story = {
    args: {
        currentPlayer: "white",
        gameStatus: "playing",
        onReset: () => console.log("ゲームリセット"),
    },
};

export const BlackWins: Story = {
    args: {
        currentPlayer: "black",
        gameStatus: "black_win",
        onReset: () => console.log("ゲームリセット"),
    },
};

export const WhiteWins: Story = {
    args: {
        currentPlayer: "white",
        gameStatus: "white_win",
        onReset: () => console.log("ゲームリセット"),
    },
};

export const Check: Story = {
    args: {
        currentPlayer: "white",
        gameStatus: "check",
        onReset: () => console.log("ゲームリセット"),
    },
};

export const Draw: Story = {
    args: {
        currentPlayer: "black",
        gameStatus: "draw",
        onReset: () => console.log("ゲームリセット"),
    },
};
