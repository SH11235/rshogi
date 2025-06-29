import { beforeEach, describe, expect, it } from "vitest";
import { modernInitialBoard } from "../initialBoard";
import type { Board } from "../model/board";
import type { PieceType } from "../model/piece";
import { initialHands } from "./moveService";
import type { Hands } from "./moveService";
import { validateReceivedMove } from "./moveValidationService";

describe("validateReceivedMove", () => {
    let board: Board;
    let hands: Hands;

    beforeEach(() => {
        board = structuredClone(modernInitialBoard);
        hands = structuredClone(initialHands());
    });

    describe("座標の検証", () => {
        it("有効な座標を受け入れる", () => {
            const result = validateReceivedMove(board, hands, "black", {
                from: "77",
                to: "67", // 歩の前進
            });
            if (!result.valid) {
                console.log("Validation error:", result.error);
            }
            expect(result.valid).toBe(true);
        });

        it("無効な移動先座標を拒否する", () => {
            const result = validateReceivedMove(board, hands, "black", {
                from: "77",
                to: "00", // 無効な座標
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("移動先の座標が不正");
        });

        it("無効な移動元座標を拒否する", () => {
            const result = validateReceivedMove(board, hands, "black", {
                from: "00", // 無効な座標
                to: "76",
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("移動元の座標が不正");
        });

        it("範囲外の座標を拒否する", () => {
            const result = validateReceivedMove(board, hands, "black", {
                from: "99",
                to: "10", // 範囲外
            });
            expect(result.valid).toBe(false);
        });
    });

    describe("通常の移動の検証", () => {
        it("合法な歩の前進を受け入れる", () => {
            const result = validateReceivedMove(board, hands, "black", {
                from: "77",
                to: "67", // 歩の前進
            });
            expect(result.valid).toBe(true);
        });

        it("存在しない駒の移動を拒否する", () => {
            const result = validateReceivedMove(board, hands, "black", {
                from: "55", // 空のマス
                to: "54",
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("移動元に駒が存在しません");
        });

        it("相手の駒の移動を拒否する", () => {
            const result = validateReceivedMove(board, hands, "black", {
                from: "33", // 後手の歩
                to: "34",
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("相手の駒は動かせません");
        });

        it("不正な移動を拒否する", () => {
            const result = validateReceivedMove(board, hands, "black", {
                from: "77",
                to: "57", // 歩は2マス進めない
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("その位置には移動できません");
        });

        it("既に成っている駒の成りを拒否する", () => {
            // テスト用の盤面を作成（成り駒を配置）
            const testBoard: Board = { ...board };
            testBoard["55"] = { type: "bishop", owner: "black", promoted: true };

            const result = validateReceivedMove(testBoard, hands, "black", {
                from: "55",
                to: "44",
                promote: true,
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("既に成っている駒です");
        });
    });

    describe("駒打ちの検証", () => {
        it("持ち駒がある場合の打ちを受け入れる", () => {
            // 銀を持ち駒に追加（歩より制約が少ない）
            const testHands = structuredClone(hands);
            testHands.black.銀 = 1;

            const result = validateReceivedMove(board, testHands, "black", {
                to: "55", // 空きマスに銀を打つ
                drop: "silver",
            });
            expect(result.valid).toBe(true);
        });

        it("持ち駒がない場合の打ちを拒否する", () => {
            const result = validateReceivedMove(board, hands, "black", {
                to: "55",
                drop: "pawn",
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("その駒を持っていません");
        });

        it("不正な駒タイプを拒否する", () => {
            const result = validateReceivedMove(board, hands, "black", {
                to: "55",
                // @ts-expect-error テスト用に無効な駒タイプを指定
                drop: "invalid" as PieceType,
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("不正な駒タイプ");
        });

        it("二歩を拒否する", () => {
            const testHands = structuredClone(hands);
            testHands.black.歩 = 1;

            // 7筋に既に歩があるので打てない
            const result = validateReceivedMove(board, testHands, "black", {
                to: "57", // 7筋には既に77に歩がある
                drop: "pawn",
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("その位置には打てません");
        });
    });

    describe("データの完全性", () => {
        it("不完全な移動データを拒否する", () => {
            const result = validateReceivedMove(board, hands, "black", {
                to: "76",
                // fromもdropもない
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("移動データが不完全");
        });

        it("空のfromを持つ移動データを拒否する", () => {
            const result = validateReceivedMove(board, hands, "black", {
                from: "",
                to: "76",
            });
            expect(result.valid).toBe(false);
            expect(result.error).toContain("移動データが不完全");
        });
    });
});
