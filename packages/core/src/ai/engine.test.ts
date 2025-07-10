import { describe, expect, it } from "vitest";
import type { Piece } from "../domain/model/piece";
import {
    type InternalAIConfig,
    canPromotePieceType,
    createInitialConfig,
    isPromotablePosition,
    keyToSquare,
    squareToKey,
    updateAIConfig,
} from "./engine";

describe("AIEngine helper functions", () => {
    describe("squareToKey", () => {
        it("should convert row and column to board key", () => {
            expect(squareToKey(1, 1)).toBe("11");
            expect(squareToKey(5, 5)).toBe("55");
            expect(squareToKey(9, 9)).toBe("99");
        });
    });

    describe("keyToSquare", () => {
        it("should convert board key to row and column", () => {
            expect(keyToSquare("11")).toEqual({ row: 1, column: 1 });
            expect(keyToSquare("55")).toEqual({ row: 5, column: 5 });
            expect(keyToSquare("99")).toEqual({ row: 9, column: 9 });
        });
    });

    describe("canPromotePieceType", () => {
        const createPiece = (type: Piece["type"], promoted = false): Piece => ({
            type,
            owner: "black",
            promoted,
        });

        it("should return true for promotable pieces", () => {
            expect(canPromotePieceType(createPiece("pawn"))).toBe(true);
            expect(canPromotePieceType(createPiece("lance"))).toBe(true);
            expect(canPromotePieceType(createPiece("knight"))).toBe(true);
            expect(canPromotePieceType(createPiece("silver"))).toBe(true);
            expect(canPromotePieceType(createPiece("bishop"))).toBe(true);
            expect(canPromotePieceType(createPiece("rook"))).toBe(true);
        });

        it("should return false for non-promotable pieces", () => {
            expect(canPromotePieceType(createPiece("king"))).toBe(false);
            expect(canPromotePieceType(createPiece("gold"))).toBe(false);
        });

        it("should return false for already promoted pieces", () => {
            expect(canPromotePieceType(createPiece("pawn", true))).toBe(false);
            expect(canPromotePieceType(createPiece("silver", true))).toBe(false);
        });
    });

    describe("isPromotablePosition", () => {
        describe("for black player", () => {
            it("should return true when moving from promotion zone", () => {
                expect(isPromotablePosition({ row: 1 }, { row: 4 }, "black")).toBe(true);
                expect(isPromotablePosition({ row: 2 }, { row: 5 }, "black")).toBe(true);
                expect(isPromotablePosition({ row: 3 }, { row: 6 }, "black")).toBe(true);
            });

            it("should return true when moving to promotion zone", () => {
                expect(isPromotablePosition({ row: 4 }, { row: 1 }, "black")).toBe(true);
                expect(isPromotablePosition({ row: 5 }, { row: 2 }, "black")).toBe(true);
                expect(isPromotablePosition({ row: 6 }, { row: 3 }, "black")).toBe(true);
            });

            it("should return false when not involving promotion zone", () => {
                expect(isPromotablePosition({ row: 4 }, { row: 5 }, "black")).toBe(false);
                expect(isPromotablePosition({ row: 6 }, { row: 7 }, "black")).toBe(false);
            });
        });

        describe("for white player", () => {
            it("should return true when moving from promotion zone", () => {
                expect(isPromotablePosition({ row: 7 }, { row: 4 }, "white")).toBe(true);
                expect(isPromotablePosition({ row: 8 }, { row: 5 }, "white")).toBe(true);
                expect(isPromotablePosition({ row: 9 }, { row: 6 }, "white")).toBe(true);
            });

            it("should return true when moving to promotion zone", () => {
                expect(isPromotablePosition({ row: 4 }, { row: 7 }, "white")).toBe(true);
                expect(isPromotablePosition({ row: 5 }, { row: 8 }, "white")).toBe(true);
                expect(isPromotablePosition({ row: 6 }, { row: 9 }, "white")).toBe(true);
            });

            it("should return false when not involving promotion zone", () => {
                expect(isPromotablePosition({ row: 4 }, { row: 5 }, "white")).toBe(false);
                expect(isPromotablePosition({ row: 2 }, { row: 3 }, "white")).toBe(false);
            });
        });
    });
});

describe("AIEngine configuration functions", () => {
    describe("createInitialConfig", () => {
        it("should create config for beginner difficulty", () => {
            const config = createInitialConfig("beginner");
            expect(config.difficulty).toBe("beginner");
            expect(config.searchDepth).toBe(2); // From AI_DIFFICULTY_CONFIGS
            expect(config.timeLimit).toBe(1000);
            expect(config.useOpeningBook).toBe(false);
        });

        it("should create config for intermediate difficulty", () => {
            const config = createInitialConfig("intermediate");
            expect(config.difficulty).toBe("intermediate");
            expect(config.searchDepth).toBe(4);
            expect(config.timeLimit).toBe(3000);
            expect(config.useOpeningBook).toBe(true);
        });

        it("should create config for advanced difficulty", () => {
            const config = createInitialConfig("advanced");
            expect(config.difficulty).toBe("advanced");
            expect(config.searchDepth).toBe(6);
            expect(config.timeLimit).toBe(5000);
            expect(config.useOpeningBook).toBe(true);
        });

        it("should create config for expert difficulty", () => {
            const config = createInitialConfig("expert");
            expect(config.difficulty).toBe("expert");
            expect(config.searchDepth).toBe(8);
            expect(config.timeLimit).toBe(30000);
            expect(config.useOpeningBook).toBe(true);
        });
    });

    describe("updateAIConfig", () => {
        const baseConfig: InternalAIConfig = {
            difficulty: "intermediate",
            searchDepth: 4,
            timeLimit: 3000,
            useOpeningBook: true,
        };

        it("should update difficulty and related settings", () => {
            const updated = updateAIConfig(baseConfig, { difficulty: "expert" });
            expect(updated.difficulty).toBe("expert");
            expect(updated.searchDepth).toBe(8); // Updated from difficulty config
            expect(updated.timeLimit).toBe(30000); // Updated from difficulty config
            expect(updated.useOpeningBook).toBe(true);
        });

        it("should update individual settings without changing difficulty", () => {
            const updated = updateAIConfig(baseConfig, {
                searchDepth: 10,
                timeLimit: 15000,
            });
            expect(updated.difficulty).toBe("intermediate");
            expect(updated.searchDepth).toBe(10);
            expect(updated.timeLimit).toBe(15000);
            expect(updated.useOpeningBook).toBe(true);
        });

        it("should disable opening book for beginner difficulty", () => {
            const updated = updateAIConfig(baseConfig, { difficulty: "beginner" });
            expect(updated.useOpeningBook).toBe(false);
        });

        it("should allow explicit useOpeningBook override", () => {
            const updated = updateAIConfig(baseConfig, { useOpeningBook: false });
            expect(updated.useOpeningBook).toBe(false);
            expect(updated.difficulty).toBe("intermediate");
        });

        it("should return new object (immutability)", () => {
            const updated = updateAIConfig(baseConfig, { searchDepth: 5 });
            expect(updated).not.toBe(baseConfig);
            expect(baseConfig.searchDepth).toBe(4); // Original unchanged
            expect(updated.searchDepth).toBe(5); // New value
        });

        it("should handle empty updates", () => {
            const updated = updateAIConfig(baseConfig, {});
            expect(updated).toEqual(baseConfig);
            expect(updated).not.toBe(baseConfig); // Still a new object
        });
    });
});
