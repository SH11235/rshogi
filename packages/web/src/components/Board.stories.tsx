import type { Meta, StoryObj } from "@storybook/react";
import { type Board as BoardType, initialBoard } from "shogi-core";
import { Board } from "./Board";

const meta: Meta<typeof Board> = {
    title: "Shogi/Board",
    component: Board,
    parameters: {
        layout: "centered",
    },
    tags: ["autodocs"],
};

export default meta;
type Story = StoryObj<typeof meta>;

export const InitialPosition: Story = {
    args: {
        board: initialBoard,
        selectedSquare: null,
        validMoves: [],
        onSquareClick: (square) => {
            console.log("Square clicked:", square);
        },
    },
};

export const WithSelectedSquare: Story = {
    args: {
        board: initialBoard,
        selectedSquare: { row: 7, column: 7 },
        validMoves: [
            { row: 6, column: 7 },
            { row: 5, column: 7 },
        ],
        onSquareClick: (square) => {
            console.log("Square clicked:", square);
        },
    },
};

export const EmptyBoard: Story = {
    args: {
        board: {} as BoardType,
        selectedSquare: null,
        validMoves: [],
        onSquareClick: (square) => {
            console.log("Square clicked:", square);
        },
    },
};
