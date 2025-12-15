import { describe, expect, it } from "vitest";
import { determinePromotion } from "./promotionLogic";

describe("determinePromotion", () => {
    it("両方の手が合法な場合は 'optional' を返す", () => {
        const legalMoves = new Set(["7g7f", "7g7f+"]);
        expect(determinePromotion(legalMoves, "7g", "7f")).toBe("optional");
    });

    it("成りのみが合法な場合は 'forced' を返す", () => {
        const legalMoves = new Set(["2c2b+"]);
        expect(determinePromotion(legalMoves, "2c", "2b")).toBe("forced");
    });

    it("成れない場合は 'none' を返す", () => {
        const legalMoves = new Set(["7g7f"]);
        expect(determinePromotion(legalMoves, "7g", "7f")).toBe("none");
    });

    it("該当する手が存在しない場合は 'none' を返す", () => {
        const legalMoves = new Set(["5g5f"]);
        expect(determinePromotion(legalMoves, "7g", "7f")).toBe("none");
    });

    it("複数の合法手がある中で正しく判定する", () => {
        const legalMoves = new Set(["7g7f", "7g7f+", "2g2f", "3g3f"]);
        expect(determinePromotion(legalMoves, "7g", "7f")).toBe("optional");
        expect(determinePromotion(legalMoves, "2g", "2f")).toBe("none");
    });
});
