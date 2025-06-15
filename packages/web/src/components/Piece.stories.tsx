import type { Meta, StoryObj } from "@storybook/react";
import { createPiece } from "shogi-core";
import { Piece } from "./Piece";

const meta: Meta<typeof Piece> = {
    title: "Shogi/Piece",
    component: Piece,
    parameters: {
        layout: "centered",
    },
    tags: ["autodocs"],
};

export default meta;
type Story = StoryObj<typeof meta>;

export const BlackPawn: Story = {
    args: {
        piece: createPiece("pawn", "black"),
    },
};

export const WhitePawn: Story = {
    args: {
        piece: createPiece("pawn", "white"),
    },
};

export const BlackKing: Story = {
    args: {
        piece: createPiece("king", "black"),
    },
};

export const WhiteKing: Story = {
    args: {
        piece: createPiece("gyoku", "white"),
    },
};

export const PromotedPawn: Story = {
    args: {
        piece: createPiece("pawn", "black", true),
    },
};

export const BlackRook: Story = {
    args: {
        piece: createPiece("rook", "black"),
    },
};

export const PromotedRook: Story = {
    args: {
        piece: createPiece("rook", "black", true),
    },
};

export const BlackBishop: Story = {
    args: {
        piece: createPiece("bishop", "black"),
    },
};

export const PromotedBishop: Story = {
    args: {
        piece: createPiece("bishop", "black", true),
    },
};

export const AllBlackPieces: Story = {
    render: () => (
        <div className="flex flex-wrap gap-2">
            <Piece piece={createPiece("pawn", "black")} />
            <Piece piece={createPiece("lance", "black")} />
            <Piece piece={createPiece("knight", "black")} />
            <Piece piece={createPiece("silver", "black")} />
            <Piece piece={createPiece("gold", "black")} />
            <Piece piece={createPiece("bishop", "black")} />
            <Piece piece={createPiece("rook", "black")} />
            <Piece piece={createPiece("king", "black")} />
        </div>
    ),
};

export const AllWhitePieces: Story = {
    render: () => (
        <div className="flex flex-wrap gap-2">
            <Piece piece={createPiece("pawn", "white")} />
            <Piece piece={createPiece("lance", "white")} />
            <Piece piece={createPiece("knight", "white")} />
            <Piece piece={createPiece("silver", "white")} />
            <Piece piece={createPiece("gold", "white")} />
            <Piece piece={createPiece("bishop", "white")} />
            <Piece piece={createPiece("rook", "white")} />
            <Piece piece={createPiece("gyoku", "white")} />
        </div>
    ),
};
