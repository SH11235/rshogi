import { initialHands, modernInitialBoard } from "shogi-core";
import { describe, expect, it } from "vitest";
import { AIEngine } from "./engine";

describe("AIEngine", () => {
    it("should initialize with correct difficulty settings", () => {
        const engine = new AIEngine("beginner");
        expect(engine).toBeDefined();
    });

    it("should evaluate initial position", () => {
        const engine = new AIEngine("beginner");
        const evaluation = engine.evaluatePosition(modernInitialBoard, initialHands(), "black");
        expect(evaluation).toMatchObject({
            score: expect.any(Number),
            depth: 0,
            pv: [],
            nodes: 0,
            time: 0,
        });
    });

    it("should calculate a legal move", async () => {
        const engine = new AIEngine("beginner");
        const move = await engine.calculateBestMove(
            modernInitialBoard,
            initialHands(),
            "black",
            [],
        );
        expect(move).toBeDefined();
        expect(move.type).toMatch(/^(move|drop)$/);
    });

    it("should respect difficulty settings for search depth", () => {
        const beginnerEngine = new AIEngine("beginner");
        const expertEngine = new AIEngine("expert");

        // Use private property access for testing
        expect((beginnerEngine as any).searchDepth).toBe(2);
        expect((expertEngine as any).searchDepth).toBe(8);
    });
});
