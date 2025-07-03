import { initialHands, modernInitialBoard } from "shogi-core";
import type { Board, Hands, Square } from "shogi-core";
import { describe, expect, it } from "vitest";
import {
    PIECE_BASE_VALUES,
    PROMOTION_BONUS,
    countAttacks,
    evaluateKingSafety,
    evaluateMobility,
    evaluatePieceCoordination,
    evaluatePosition,
    getPositionBonus,
} from "./evaluation";

describe("evaluation", () => {
    describe("piece values", () => {
        it("should have correct base values", () => {
            expect(PIECE_BASE_VALUES.pawn).toBe(100);
            expect(PIECE_BASE_VALUES.lance).toBe(430);
            expect(PIECE_BASE_VALUES.knight).toBe(450);
            expect(PIECE_BASE_VALUES.silver).toBe(640);
            expect(PIECE_BASE_VALUES.gold).toBe(690);
            expect(PIECE_BASE_VALUES.bishop).toBe(890);
            expect(PIECE_BASE_VALUES.rook).toBe(1040);
            expect(PIECE_BASE_VALUES.king).toBe(0);
        });

        it("should have correct promotion bonuses", () => {
            expect(PROMOTION_BONUS.pawn).toBe(420);
            expect(PROMOTION_BONUS.lance).toBe(260);
            expect(PROMOTION_BONUS.knight).toBe(240);
            expect(PROMOTION_BONUS.silver).toBe(50);
            expect(PROMOTION_BONUS.gold).toBe(0);
            expect(PROMOTION_BONUS.bishop).toBe(150);
            expect(PROMOTION_BONUS.rook).toBe(150);
        });
    });

    describe("position bonus", () => {
        it("should give position bonus for pawns", () => {
            const blackPawn = { type: "pawn" as const, owner: "black" as const, promoted: false };
            const whitePawn = { type: "pawn" as const, owner: "white" as const, promoted: false };

            // 前進した歩は高評価
            expect(getPositionBonus(blackPawn, "33")).toBeGreaterThan(
                getPositionBonus(blackPawn, "77"),
            );
            expect(getPositionBonus(whitePawn, "77")).toBeGreaterThan(
                getPositionBonus(whitePawn, "33"),
            );
        });

        it("should give position bonus for other pieces", () => {
            const blackSilver = {
                type: "silver" as const,
                owner: "black" as const,
                promoted: false,
            };

            // 中央の銀は高評価
            expect(getPositionBonus(blackSilver, "55")).toBeGreaterThan(
                getPositionBonus(blackSilver, "91"),
            );
        });
    });

    describe("countAttacks", () => {
        it("should count attacks on a square", () => {
            const board: Board = {
                ...Object.fromEntries(Object.keys(modernInitialBoard).map((key) => [key, null])),
                "55": { type: "gold", owner: "black", promoted: false },
                "54": { type: "pawn", owner: "black", promoted: false },
                "56": { type: "silver", owner: "black", promoted: false },
            };

            const targetSquare: Square = { row: 4, column: 5 };
            const attacks = countAttacks(board, targetSquare, "black");
            expect(attacks).toBeGreaterThan(0);
        });
    });

    describe("evaluateKingSafety", () => {
        it("should penalize king in check", () => {
            const boardInCheck: Board = {
                ...Object.fromEntries(Object.keys(modernInitialBoard).map((key) => [key, null])),
                "59": { type: "king", owner: "black", promoted: false },
                "51": { type: "rook", owner: "white", promoted: false },
            };

            const safetyInCheck = evaluateKingSafety(boardInCheck, "black");
            expect(safetyInCheck).toBeLessThan(0);
        });

        it("should reward king with defenders", () => {
            const boardWithDefenders: Board = {
                ...Object.fromEntries(Object.keys(modernInitialBoard).map((key) => [key, null])),
                "59": { type: "king", owner: "black", promoted: false },
                "58": { type: "gold", owner: "black", promoted: false },
                "68": { type: "gold", owner: "black", promoted: false },
                "69": { type: "silver", owner: "black", promoted: false },
            };

            const safetyWithDefenders = evaluateKingSafety(boardWithDefenders, "black");
            expect(safetyWithDefenders).toBeGreaterThan(0);
        });
    });

    describe("evaluateMobility", () => {
        it("should count legal moves", () => {
            const mobility = evaluateMobility(modernInitialBoard, "black");
            expect(mobility).toBeGreaterThan(0);
        });

        it("should weight major pieces more", () => {
            const boardWithRook: Board = {
                ...Object.fromEntries(Object.keys(modernInitialBoard).map((key) => [key, null])),
                "55": { type: "rook", owner: "black", promoted: false },
            };

            const boardWithPawn: Board = {
                ...Object.fromEntries(Object.keys(modernInitialBoard).map((key) => [key, null])),
                "55": { type: "pawn", owner: "black", promoted: false },
            };

            const rookMobility = evaluateMobility(boardWithRook, "black");
            const pawnMobility = evaluateMobility(boardWithPawn, "black");

            // 飛車の機動力は歩より高く評価される
            expect(rookMobility).toBeGreaterThan(pawnMobility);
        });
    });

    describe("evaluatePieceCoordination", () => {
        it("should reward coordinated pieces", () => {
            const coordinatedBoard: Board = {
                ...Object.fromEntries(Object.keys(modernInitialBoard).map((key) => [key, null])),
                "55": { type: "gold", owner: "black", promoted: false },
                "54": { type: "silver", owner: "black", promoted: false },
                "56": { type: "silver", owner: "black", promoted: false },
            };

            const coordination = evaluatePieceCoordination(coordinatedBoard, "black");
            expect(coordination).toBeGreaterThan(0);
        });
    });

    describe("evaluatePosition", () => {
        it("should evaluate initial position as equal", () => {
            const evaluation = evaluatePosition(modernInitialBoard, initialHands(), "black");
            // 初期局面はほぼ互角
            expect(Math.abs(evaluation.total)).toBeLessThan(100);
        });

        it("should include all evaluation components", () => {
            const evaluation = evaluatePosition(modernInitialBoard, initialHands(), "black");
            expect(evaluation).toHaveProperty("material");
            expect(evaluation).toHaveProperty("position");
            expect(evaluation).toHaveProperty("kingSafety");
            expect(evaluation).toHaveProperty("mobility");
            expect(evaluation).toHaveProperty("coordination");
            expect(evaluation).toHaveProperty("total");
        });

        it("should favor player with material advantage", () => {
            const boardWithExtraRook: Board = {
                ...modernInitialBoard,
                "55": { type: "rook", owner: "black", promoted: false },
            };

            const evaluation = evaluatePosition(boardWithExtraRook, initialHands(), "black");
            expect(evaluation.total).toBeGreaterThan(500);
        });

        it("should evaluate promoted pieces correctly", () => {
            const boardWithPromotion: Board = {
                ...Object.fromEntries(Object.keys(modernInitialBoard).map((key) => [key, null])),
                "33": { type: "pawn", owner: "black", promoted: true }, // と金
                "77": { type: "pawn", owner: "black", promoted: false }, // 歩
            };

            const evaluation = evaluatePosition(boardWithPromotion, initialHands(), "black");
            // と金の方が価値が高い
            expect(evaluation.material).toBeGreaterThan(PIECE_BASE_VALUES.pawn);
        });

        it("should evaluate hands correctly", () => {
            const hands: Hands = {
                black: { pawn: 2, lance: 1, knight: 0, silver: 0, gold: 0, bishop: 0, rook: 0 },
                white: { pawn: 0, lance: 0, knight: 0, silver: 0, gold: 0, bishop: 0, rook: 0 },
            };

            const evaluation = evaluatePosition(modernInitialBoard, hands, "black");
            // 持ち駒があるので有利
            expect(evaluation.material).toBeGreaterThan(0);
        });
    });
});
