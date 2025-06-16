import type { Meta, StoryObj } from "@storybook/react";
import { CapturedPieces } from "./CapturedPieces";

const meta: Meta<typeof CapturedPieces> = {
    title: "Shogi/CapturedPieces",
    component: CapturedPieces,
    parameters: {
        layout: "centered",
    },
    tags: ["autodocs"],
    argTypes: {
        player: {
            control: { type: "select" },
            options: ["black", "white"],
        },
    },
};

export default meta;
type Story = StoryObj<typeof meta>;

const emptyHands = {
    black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
    white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
};

const sampleHands = {
    black: { 歩: 3, 香: 1, 桂: 0, 銀: 1, 金: 0, 角: 0, 飛: 1 },
    white: { 歩: 2, 香: 0, 桂: 1, 銀: 0, 金: 1, 角: 1, 飛: 0 },
};

const fullHands = {
    black: { 歩: 9, 香: 2, 桂: 2, 銀: 2, 金: 2, 角: 1, 飛: 1 },
    white: { 歩: 9, 香: 2, 桂: 2, 銀: 2, 金: 2, 角: 1, 飛: 1 },
};

export const BlackEmpty: Story = {
    args: {
        hands: emptyHands,
        player: "black",
        currentPlayer: "black",
        selectedDropPiece: null,
    },
};

export const WhiteEmpty: Story = {
    args: {
        hands: emptyHands,
        player: "white",
        currentPlayer: "black",
        selectedDropPiece: null,
    },
};

export const BlackWithPieces: Story = {
    args: {
        hands: sampleHands,
        player: "black",
        currentPlayer: "black",
        selectedDropPiece: null,
    },
};

export const WhiteWithPieces: Story = {
    args: {
        hands: sampleHands,
        player: "white",
        currentPlayer: "black",
        selectedDropPiece: null,
    },
};

export const BlackFull: Story = {
    args: {
        hands: fullHands,
        player: "black",
        currentPlayer: "black",
        selectedDropPiece: null,
    },
};

export const WhiteFull: Story = {
    args: {
        hands: fullHands,
        player: "white",
        currentPlayer: "black",
        selectedDropPiece: null,
    },
};

export const BothPlayers: Story = {
    render: () => (
        <div className="space-y-4">
            <CapturedPieces
                hands={sampleHands}
                player="black"
                currentPlayer="black"
                selectedDropPiece={null}
            />
            <CapturedPieces
                hands={sampleHands}
                player="white"
                currentPlayer="black"
                selectedDropPiece={null}
            />
        </div>
    ),
};
