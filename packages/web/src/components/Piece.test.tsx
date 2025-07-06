import { render } from "@testing-library/react";
import { createPiece } from "shogi-core";
import { describe, expect, it } from "vitest";
import { Piece } from "./Piece";

describe("Piece component", () => {
    it("renders black pawn correctly", () => {
        const piece = createPiece("pawn", "black");
        const { container } = render(<Piece piece={piece} />);

        expect(container.textContent).toBe("歩");
        // Text is now in a nested div, not firstChild
        const textElement = container.querySelector(".text-black");
        expect(textElement).toBeTruthy();
        // SVG element should not be rotated for black pieces
        const svgElement = container.querySelector("svg");
        expect(svgElement).not.toHaveClass("rotate-180");
    });

    it("renders white pawn correctly", () => {
        const piece = createPiece("pawn", "white");
        const { container } = render(<Piece piece={piece} />);

        expect(container.textContent).toBe("歩");
        // Text color is in nested div
        const textElement = container.querySelector(".text-red-600");
        expect(textElement).toBeTruthy();
        // SVG should be rotated for white pieces
        const svgElement = container.querySelector("svg");
        expect(svgElement).toHaveClass("rotate-180");
    });

    it("renders promoted pawn correctly", () => {
        const piece = createPiece("pawn", "black", true);
        const { container } = render(<Piece piece={piece} />);

        expect(container.textContent).toBe("と");
        // Background color is now on polygon fill, check if promoted
        const polygon = container.querySelector("polygon");
        expect(polygon).toHaveClass("fill-[#ffd700]");
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
            { type: "silver", expected: "全" }, // Updated to match PROMOTED_PIECE_NAMES
            { type: "knight", expected: "圭" }, // Updated to match PROMOTED_PIECE_NAMES
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

        // Check polygon fill for promoted pieces
        const polygon = container.querySelector("polygon");
        expect(polygon).toHaveClass("fill-[#ffd700]");
    });

    it("applies correct styling for white pieces", () => {
        const piece = createPiece("king", "white");
        const { container } = render(<Piece piece={piece} />);

        // Check text color in nested div
        const textElement = container.querySelector(".text-red-600");
        expect(textElement).toBeTruthy();
        expect(textElement).not.toHaveClass("text-black");
        // Check SVG rotation
        const svgElement = container.querySelector("svg");
        expect(svgElement).toHaveClass("rotate-180");
    });

    it("renders promoted lance with vertical text", () => {
        const piece = createPiece("lance", "black", true);
        const { container } = render(<Piece piece={piece} />);

        // Check for vertical text structure
        const verticalTextContainer = container.querySelector(".flex-col");
        expect(verticalTextContainer).toBeTruthy();
        expect(verticalTextContainer?.textContent).toBe("成香");

        // Check individual characters
        const spans = verticalTextContainer?.querySelectorAll("span");
        expect(spans?.length).toBe(2);
        expect(spans?.[0].textContent).toBe("成");
        expect(spans?.[1].textContent).toBe("香");
    });
});
