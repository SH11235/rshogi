import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { CapturedPieces } from "./CapturedPieces";

const defaultProps = {
    currentPlayer: "black" as const,
    selectedDropPiece: null,
    onPieceClick: undefined,
};

describe("CapturedPieces component", () => {
    const emptyHands = {
        black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
        white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
    };

    const sampleHands = {
        black: { 歩: 3, 香: 1, 桂: 0, 銀: 1, 金: 0, 角: 0, 飛: 1 },
        white: { 歩: 2, 香: 0, 桂: 1, 銀: 0, 金: 1, 角: 1, 飛: 0 },
    };

    it("displays black player title correctly", () => {
        render(<CapturedPieces {...defaultProps} hands={emptyHands} player="black" />);

        expect(screen.getByText("先手の持ち駒")).toBeInTheDocument();
    });

    it("displays white player title correctly", () => {
        render(<CapturedPieces {...defaultProps} hands={emptyHands} player="white" />);

        expect(screen.getByText("後手の持ち駒")).toBeInTheDocument();
    });

    it("shows 'なし' when no pieces are captured", () => {
        render(<CapturedPieces {...defaultProps} hands={emptyHands} player="black" />);

        expect(screen.getByText("なし")).toBeInTheDocument();
    });

    it("displays captured pieces correctly", () => {
        render(<CapturedPieces {...defaultProps} hands={sampleHands} player="black" />);

        // Should show pieces with counts > 0
        expect(screen.getByText("飛")).toBeInTheDocument();
        expect(screen.getByText("銀")).toBeInTheDocument();
        expect(screen.getByText("歩")).toBeInTheDocument();
        expect(screen.getByText("×3")).toBeInTheDocument(); // 歩×3
        expect(screen.getByText("香")).toBeInTheDocument();

        // Pieces with count 0 should not be rendered
        expect(screen.queryByText("桂")).not.toBeInTheDocument();
        expect(screen.queryByText("金")).not.toBeInTheDocument();
        expect(screen.queryByText("角")).not.toBeInTheDocument();

        // 'なし' should not be shown when there are captured pieces
        expect(screen.queryByText("なし")).not.toBeInTheDocument();
    });

    it("displays piece counts correctly", () => {
        render(<CapturedPieces {...defaultProps} hands={sampleHands} player="white" />);

        expect(screen.getByText("×2")).toBeInTheDocument(); // 歩×2
        expect(screen.getByText("桂")).toBeInTheDocument(); // 桂×1 (no count shown for 1)
        expect(screen.getByText("金")).toBeInTheDocument();
        expect(screen.getByText("角")).toBeInTheDocument();
    });

    it("does not show count for single pieces", () => {
        const handsWithSingle = {
            black: { 歩: 1, 香: 1, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
            white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
        };

        render(<CapturedPieces {...defaultProps} hands={handsWithSingle} player="black" />);

        expect(screen.getByText("歩")).toBeInTheDocument();
        expect(screen.getByText("香")).toBeInTheDocument();
        expect(screen.queryByText("×1")).not.toBeInTheDocument();
    });

    it("applies correct styling for black player", () => {
        render(<CapturedPieces {...defaultProps} hands={emptyHands} player="black" />);

        const title = screen.getByText("先手の持ち駒");
        expect(title).toHaveClass("text-black");
        expect(title).not.toHaveClass("text-red-600");
    });

    it("applies correct styling for white player", () => {
        render(<CapturedPieces {...defaultProps} hands={emptyHands} player="white" />);

        const title = screen.getByText("後手の持ち駒");
        expect(title).toHaveClass("text-red-600");
        expect(title).not.toHaveClass("text-black");
    });

    it("applies rotation for white player", () => {
        const { container } = render(
            <CapturedPieces {...defaultProps} hands={emptyHands} player="white" />,
        );

        expect(container.firstChild).toHaveClass("rotate-180");
    });

    it("does not apply rotation for black player", () => {
        const { container } = render(
            <CapturedPieces {...defaultProps} hands={emptyHands} player="black" />,
        );

        expect(container.firstChild).not.toHaveClass("rotate-180");
    });

    it("displays pieces in correct order", () => {
        render(<CapturedPieces {...defaultProps} hands={sampleHands} player="black" />);

        const pieces = screen.getAllByText(/[飛角金銀桂香歩]/);
        const pieceTexts = pieces.map((el) => el.textContent);

        // Check that pieces appear in the expected order (飛, 角, 金, 銀, 桂, 香, 歩)
        const expectedOrder = ["飛", "銀", "香", "歩"];
        for (const piece of expectedOrder) {
            expect(pieceTexts).toContain(piece);
        }
    });
});
