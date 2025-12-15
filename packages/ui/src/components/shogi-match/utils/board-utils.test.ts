import type { Hands, PositionState, Square } from "@shogi/app-core";
import { describe, expect, it } from "vitest";
import { addToHand, cloneHandsState, consumeFromHand, countPieces } from "./boardUtils";

// テスト用のヘルパー関数
const createEmptyHands = (): Hands => ({ sente: {}, gote: {} });

const getAllSquares = (): Square[] => {
    const squares: Square[] = [];
    const files = ["9", "8", "7", "6", "5", "4", "3", "2", "1"];
    const ranks = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    for (const rank of ranks) {
        for (const file of files) {
            squares.push(`${file}${rank}` as Square);
        }
    }
    return squares;
};

describe("boardUtils", () => {
    describe("cloneHandsState", () => {
        it("持ち駒を正しくクローンする", () => {
            const hands: Hands = {
                sente: { P: 3, L: 1 },
                gote: { R: 1 },
            };
            const cloned = cloneHandsState(hands);

            expect(cloned).toEqual(hands);
            expect(cloned).not.toBe(hands);
            expect(cloned.sente).not.toBe(hands.sente);
            expect(cloned.gote).not.toBe(hands.gote);
        });

        it("空の持ち駒をクローンする", () => {
            const hands = createEmptyHands();
            const cloned = cloneHandsState(hands);

            expect(cloned).toEqual(hands);
            expect(cloned).not.toBe(hands);
        });
    });

    describe("addToHand", () => {
        it("持ち駒に駒を追加する", () => {
            const hands: Hands = {
                sente: { P: 1 },
                gote: {},
            };
            const newHands = addToHand(hands, "sente", "P");

            expect(newHands.sente.P).toBe(2);
            expect(hands.sente.P).toBe(1); // 元の持ち駒は変更されない
        });

        it("新しい種類の駒を追加する", () => {
            const hands: Hands = {
                sente: { P: 1 },
                gote: {},
            };
            const newHands = addToHand(hands, "sente", "R");

            expect(newHands.sente.R).toBe(1);
            expect(newHands.sente.P).toBe(1);
        });

        it("後手に駒を追加する", () => {
            const hands = createEmptyHands();
            const newHands = addToHand(hands, "gote", "B");

            expect(newHands.gote.B).toBe(1);
        });
    });

    describe("consumeFromHand", () => {
        it("持ち駒から駒を消費する", () => {
            const hands: Hands = {
                sente: { P: 3 },
                gote: {},
            };
            const newHands = consumeFromHand(hands, "sente", "P");

            expect(newHands).not.toBeNull();
            const assertedHands = newHands as Hands;
            expect(assertedHands.sente.P).toBe(2);
            expect(hands.sente.P).toBe(3); // 元の持ち駒は変更されない
        });

        it("最後の1枚を消費するとプロパティが削除される", () => {
            const hands: Hands = {
                sente: { P: 1 },
                gote: {},
            };
            const newHands = consumeFromHand(hands, "sente", "P");

            expect(newHands).not.toBeNull();
            const assertedHands = newHands as Hands;
            expect(assertedHands.sente.P).toBeUndefined();
        });

        it("存在しない駒を消費しようとすると null を返す", () => {
            const hands = createEmptyHands();
            const newHands = consumeFromHand(hands, "sente", "R");

            expect(newHands).toBeNull();
        });

        it("0枚の駒を消費しようとすると null を返す", () => {
            const hands: Hands = {
                sente: { P: 0 },
                gote: {},
            };
            const newHands = consumeFromHand(hands, "sente", "P");

            expect(newHands).toBeNull();
        });
    });

    describe("countPieces", () => {
        it("盤上と持ち駒の駒数を正しくカウントする", () => {
            const position: PositionState = {
                board: Object.fromEntries(
                    getAllSquares().map((sq) => {
                        // 9a に先手の歩、1a に後手の玉を配置
                        if (sq === "9a") {
                            return [sq, { owner: "sente", type: "P" }];
                        }
                        if (sq === "1a") {
                            return [sq, { owner: "gote", type: "K" }];
                        }
                        return [sq, null];
                    }),
                ) as PositionState["board"],
                hands: {
                    sente: { P: 5, R: 1 },
                    gote: { B: 1 },
                },
                turn: "sente",
                ply: 1,
            };

            const counts = countPieces(position);

            expect(counts.sente.P).toBe(6); // 盤上1枚 + 持ち駒5枚
            expect(counts.sente.R).toBe(1);
            expect(counts.gote.K).toBe(1);
            expect(counts.gote.B).toBe(1);
            expect(counts.sente.K).toBe(0);
        });

        it("空の盤面と持ち駒の駒数を正しくカウントする", () => {
            const position: PositionState = {
                board: Object.fromEntries(
                    getAllSquares().map((sq) => [sq, null]),
                ) as PositionState["board"],
                hands: createEmptyHands(),
                turn: "sente",
                ply: 1,
            };

            const counts = countPieces(position);

            expect(counts.sente.P).toBe(0);
            expect(counts.gote.P).toBe(0);
            // すべての駒が0であることを確認
            for (const owner of ["sente", "gote"] as const) {
                for (const type of ["K", "R", "B", "G", "S", "N", "L", "P"] as const) {
                    expect(counts[owner][type]).toBe(0);
                }
            }
        });
    });
});
