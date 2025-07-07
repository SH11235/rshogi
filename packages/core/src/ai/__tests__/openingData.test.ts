import { describe, expect, it } from "vitest";
import { generateMainOpenings } from "../openingData";

describe("generateMainOpenings", () => {
    it("should generate basic opening entries", () => {
        const openings = generateMainOpenings();

        expect(openings).toBeInstanceOf(Array);
        expect(openings.length).toBeGreaterThan(0);
        expect(openings.length).toBeLessThan(300); // 基本定跡なので300以下
    });

    it("should include initial position", () => {
        const openings = generateMainOpenings();

        // 初期局面の定跡が含まれているか
        const initialPosition = openings.find(
            (entry) =>
                entry.position === "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -",
        );

        expect(initialPosition).toBeDefined();
        expect(initialPosition?.moves.length).toBeGreaterThan(0);

        // 基本的な初手が含まれているか
        const moveNotations = initialPosition?.moves.map((m) => m.notation);
        expect(moveNotations).toContain("7g7f"); // 居飛車
        expect(moveNotations).toContain("2g2f"); // 振り飛車
    });

    it("should have valid opening entries", () => {
        const openings = generateMainOpenings();

        for (const entry of openings) {
            // 各エントリの検証
            expect(entry.position).toBeTruthy();
            expect(entry.position).toMatch(/^[lnsgkrbpLNSGKRBP1-9\/+\-bw ]+$/); // SFEN形式
            expect(entry.moves).toBeInstanceOf(Array);
            expect(entry.moves.length).toBeGreaterThan(0);
            expect(entry.depth).toBeGreaterThan(0);

            // 各手の検証
            for (const move of entry.moves) {
                expect(move.move).toBeDefined();
                expect(move.notation).toBeTruthy();
                expect(move.notation).toMatch(/^[1-9][a-i][1-9][a-i](\+)?$/); // 棋譜形式
                expect(move.weight).toBeGreaterThan(0);

                // moveオブジェクトの基本検証
                expect(move.move.type).toBe("move");
                expect(move.move.from).toBeDefined();
                expect(move.move.from.row).toBeGreaterThanOrEqual(1);
                expect(move.move.from.row).toBeLessThanOrEqual(9);
                expect(move.move.from.column).toBeGreaterThanOrEqual(1);
                expect(move.move.from.column).toBeLessThanOrEqual(9);
                expect(move.move.to).toBeDefined();
                expect(move.move.to.row).toBeGreaterThanOrEqual(1);
                expect(move.move.to.row).toBeLessThanOrEqual(9);
                expect(move.move.to.column).toBeGreaterThanOrEqual(1);
                expect(move.move.to.column).toBeLessThanOrEqual(9);
                expect(move.move.piece).toBeTruthy();
                expect(move.move.piece.type).toBeTruthy();
                expect(move.move.piece.owner).toMatch(/^(black|white)$/);
                expect(move.move.piece.promoted).toBe(false);
            }
        }
    });

    it("should include popular openings", () => {
        const openings = generateMainOpenings();

        // 矢倉の基本形への手順が含まれているか
        const hasYagura = openings.some((entry) =>
            entry.moves.some(
                (move) => move.name?.includes("矢倉") || move.comment?.includes("矢倉"),
            ),
        );

        // 四間飛車の基本形への手順が含まれているか
        const hasShikenbisha = openings.some((entry) =>
            entry.moves.some(
                (move) => move.name?.includes("四間飛車") || move.comment?.includes("四間飛車"),
            ),
        );

        expect(hasYagura || hasShikenbisha).toBe(true);
    });

    it("should have reasonable weights", () => {
        const openings = generateMainOpenings();

        for (const entry of openings) {
            const totalWeight = entry.moves.reduce((sum, move) => sum + move.weight, 0);

            // 重みの合計が妥当な範囲か
            expect(totalWeight).toBeGreaterThan(50);
            expect(totalWeight).toBeLessThan(10000);

            // 各手の重みが妥当か
            for (const move of entry.moves) {
                expect(move.weight).toBeGreaterThan(0);
                expect(move.weight).toBeLessThan(1000);
            }
        }
    });
});
