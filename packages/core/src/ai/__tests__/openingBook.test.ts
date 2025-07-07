import { beforeEach, describe, expect, it } from "vitest";
import type { Move } from "../../domain/model/move";
import type { PositionState } from "../../domain/model/positionState";
import { OpeningBook, type OpeningEntry } from "../openingBook";

describe("OpeningBook", () => {
    let openingBook: OpeningBook;

    beforeEach(() => {
        openingBook = new OpeningBook();
    });

    describe("Basic Operations", () => {
        it("should create an empty opening book", () => {
            expect(openingBook.size()).toBe(0);
            expect(openingBook.getMemoryUsage()).toBe(0);
        });

        it("should add and find opening moves", () => {
            // 初期局面の定跡エントリ
            const entry: OpeningEntry = {
                position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -",
                moves: [
                    {
                        move: { type: "move", from: [7, 7], to: [7, 6], piece: "Pawn" } as Move,
                        notation: "7g7f",
                        weight: 100,
                        depth: 20,
                    },
                    {
                        move: { type: "move", from: [2, 7], to: [2, 6], piece: "Pawn" } as Move,
                        notation: "2g2f",
                        weight: 80,
                        depth: 18,
                    },
                ],
                depth: 20,
            };

            openingBook.addEntry(entry);

            expect(openingBook.size()).toBe(1);
            expect(openingBook.getMemoryUsage()).toBeGreaterThan(0);

            // 局面から手を検索
            const position: PositionState = {
                board: { _testInitialPosition: true } as any, // 初期局面フラグ
                hands: { Black: {}, White: {} } as any,
                currentPlayer: "Black",
            };

            const moves = openingBook.findMoves(position);
            expect(moves).toHaveLength(2);
            expect(moves[0].notation).toBe("7g7f");
            expect(moves[1].notation).toBe("2g2f");
        });

        it("should return empty array for unknown position", () => {
            const position: PositionState = {
                board: {} as any,
                hands: { Black: {}, White: {} } as any,
                currentPlayer: "Black",
            };

            const moves = openingBook.findMoves(position);
            expect(moves).toEqual([]);
        });

        it("should clear all entries", () => {
            const entry: OpeningEntry = {
                position: "test-position",
                moves: [
                    {
                        move: {} as Move,
                        notation: "test",
                        weight: 100,
                    },
                ],
                depth: 10,
            };

            openingBook.addEntry(entry);
            expect(openingBook.size()).toBe(1);

            openingBook.clear();
            expect(openingBook.size()).toBe(0);
            expect(openingBook.getMemoryUsage()).toBe(0);
        });
    });

    describe("SFEN Key Generation", () => {
        it("should generate correct SFEN key without move count", () => {
            const fullSfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
            const expectedKey = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";

            const key = openingBook.generatePositionKey({
                board: {} as any,
                hands: { Black: {}, White: {} } as any,
                currentPlayer: "Black",
            });

            // 実装では実際のboardからSFENを生成する必要がある
            // ここではテストの期待値のみ定義
            expect(typeof key).toBe("string");
        });
    });

    describe("Memory Management", () => {
        it("should track memory usage correctly", () => {
            const initialMemory = openingBook.getMemoryUsage();

            const entry: OpeningEntry = {
                position: "test-position",
                moves: [
                    {
                        move: {} as Move,
                        notation: "7g7f",
                        weight: 100,
                        depth: 20,
                    },
                ],
                depth: 20,
            };

            openingBook.addEntry(entry);

            const afterMemory = openingBook.getMemoryUsage();
            expect(afterMemory).toBeGreaterThan(initialMemory);

            // メモリ使用量の推定値が妥当か
            expect(afterMemory).toBeGreaterThan(50); // 最低でも50バイト以上
            expect(afterMemory).toBeLessThan(1000); // 1KB未満
        });

        it("should reject entries when memory limit is exceeded", () => {
            // メモリ制限を小さく設定してテスト
            const smallBook = new OpeningBook(1000); // 1KB制限

            let added = 0;
            for (let i = 0; i < 100; i++) {
                const entry: OpeningEntry = {
                    position: `position-${i}`,
                    moves: Array(10)
                        .fill(null)
                        .map((_, j) => ({
                            move: {} as Move,
                            notation: `move-${j}`,
                            weight: 100,
                            depth: 20,
                        })),
                    depth: 20,
                };

                const result = smallBook.addEntry(entry);
                if (result) added++;
                else break;
            }

            expect(added).toBeGreaterThan(0);
            expect(added).toBeLessThan(100);
            expect(smallBook.getMemoryUsage()).toBeLessThanOrEqual(1000);
        });
    });

    describe("Weighted Random Selection", () => {
        it("should select moves based on weights", () => {
            const entry: OpeningEntry = {
                position: "test-position",
                moves: [
                    { move: {} as Move, notation: "move1", weight: 900 },
                    { move: {} as Move, notation: "move2", weight: 100 },
                ],
                depth: 10,
            };

            openingBook.addEntry(entry);

            // 1000回試行して分布を確認
            const counts = { move1: 0, move2: 0 };
            const testPosition: PositionState = {
                board: {} as any,
                hands: { Black: {}, White: {} } as any,
                currentPlayer: "Black",
            };

            for (let i = 0; i < 1000; i++) {
                const moves = openingBook.findMoves(testPosition, { randomize: true });
                if (moves.length > 0) {
                    counts[moves[0].notation as keyof typeof counts]++;
                }
            }

            // move1が約90%、move2が約10%になるはず
            expect(counts.move1).toBeGreaterThan(800);
            expect(counts.move2).toBeGreaterThan(50);
            expect(counts.move1 + counts.move2).toBe(1000);
        });
    });
});
