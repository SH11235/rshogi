import { render } from "@testing-library/react";
import { createPiece } from "shogi-core";
import { describe, expect, it } from "vitest";
import { Piece } from "./Piece";

describe("Piece component", () => {
    it("renders black pawn correctly", () => {
        const piece = createPiece("pawn", "black");
        const { container } = render(<Piece piece={piece} />);

        expect(container.textContent).toBe("歩");
        expect(container.firstChild).toHaveClass("text-black");
        expect(container.firstChild).not.toHaveClass("rotate-180");
    });

    it("renders white pawn correctly", () => {
        const piece = createPiece("pawn", "white");
        const { container } = render(<Piece piece={piece} />);

        expect(container.textContent).toBe("歩");
        expect(container.firstChild).toHaveClass("text-red-600", "rotate-180");
    });

    it("renders promoted pawn correctly", () => {
        const piece = createPiece("pawn", "black", true);
        const { container } = render(<Piece piece={piece} />);

        expect(container.textContent).toBe("と");
        expect(container.firstChild).toHaveClass("bg-yellow-100");
    });

    it("renders different piece types correctly", () => {
        const pieces = [
            { type: "king", expected: "王" },
            { type: "rook", expected: "飛" },
            { type: "bishop", expected: "角" },
            { type: "gold", expected: "金" },
            { type: "silver", expected: "銀" },
            { type: "knight", expected: "桂" },
            { type: "lance", expected: "香" },
        ] as const;

        for (const { type, expected } of pieces) {
            const piece = createPiece(type, "black");
            const { container } = render(<Piece piece={piece} />);
            expect(container.textContent).toBe(expected);
        }
    });

    it("renders promoted pieces correctly", () => {
        const promotedPieces = [
            { type: "rook", expected: "龍" },
            { type: "bishop", expected: "馬" },
            { type: "silver", expected: "成銀" },
            { type: "knight", expected: "成桂" },
            { type: "lance", expected: "成香" },
        ] as const;

        for (const { type, expected } of promotedPieces) {
            const piece = createPiece(type, "black", true);
            const { container } = render(<Piece piece={piece} />);
            expect(container.textContent).toBe(expected);
        }
    });

    it("applies correct styling for promoted pieces", () => {
        const piece = createPiece("bishop", "black", true);
        const { container } = render(<Piece piece={piece} />);

        expect(container.firstChild).toHaveClass("bg-yellow-100");
    });

    it("applies correct styling for white pieces", () => {
        const piece = createPiece("king", "white");
        const { container } = render(<Piece piece={piece} />);

        expect(container.firstChild).toHaveClass("text-red-600", "rotate-180");
        expect(container.firstChild).not.toHaveClass("text-black");
    });
});
