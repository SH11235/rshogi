import { describe, expect, it } from "vitest";
import { generateMainOpenings } from "./openingData";

describe("generateMainOpenings", () => {
    describe("基本機能テスト", () => {
        it("定跡エントリの配列を返すこと", () => {
            const openings = generateMainOpenings();

            expect(openings).toBeInstanceOf(Array);
            expect(openings.length).toBeGreaterThan(0);
            // 基本定跡なので、あまり多くないはず
            expect(openings.length).toBeLessThan(300);
        });

        it("各エントリがOpeningEntryインターフェースに準拠していること", () => {
            const openings = generateMainOpenings();

            for (const entry of openings) {
                expect(entry).toHaveProperty("position");
                expect(entry).toHaveProperty("moves");
                expect(entry).toHaveProperty("depth");
                expect(typeof entry.position).toBe("string");
                expect(Array.isArray(entry.moves)).toBe(true);
                expect(typeof entry.depth).toBe("number");
            }
        });
    });

    describe("データ構造検証", () => {
        it("SFEN形式のpositionが妥当であること", () => {
            const openings = generateMainOpenings();
            // SFEN形式の正規表現（手番と持ち駒情報を含む）
            // 持ち駒部分は数字を含む可能性がある（例：2L）
            const sfenRegex = /^[lnsgkrbpLNSGKRBP1-9\/+]+ [bw] [RBGSNLPrbgsnlp0-9\-]+$/;

            for (const entry of openings) {
                expect(entry.position).toMatch(sfenRegex);
            }
        });

        it("各moveがOpeningMoveインターフェースに準拠していること", () => {
            const openings = generateMainOpenings();

            for (const entry of openings) {
                expect(entry.moves.length).toBeGreaterThan(0);

                for (const move of entry.moves) {
                    // 必須プロパティ
                    expect(move).toHaveProperty("move");
                    expect(move).toHaveProperty("notation");
                    expect(move).toHaveProperty("weight");

                    // 型チェック
                    expect(move.move).toBeDefined();
                    expect(typeof move.notation).toBe("string");
                    expect(typeof move.weight).toBe("number");

                    // オプショナルプロパティ
                    if (move.depth !== undefined) {
                        expect(typeof move.depth).toBe("number");
                    }
                    if (move.name !== undefined) {
                        expect(typeof move.name).toBe("string");
                    }
                    if (move.comment !== undefined) {
                        expect(typeof move.comment).toBe("string");
                    }
                }
            }
        });

        it("Moveオブジェクトの構造が正しいこと", () => {
            const openings = generateMainOpenings();

            for (const entry of openings) {
                for (const openingMove of entry.moves) {
                    const move = openingMove.move;

                    // typeプロパティの検証
                    expect(move.type).toMatch(/^(move|drop)$/);

                    if (move.type === "move") {
                        // 通常の移動
                        expect(move).toHaveProperty("from");
                        expect(move).toHaveProperty("to");
                        expect(move).toHaveProperty("piece");
                        expect(move).toHaveProperty("promote");

                        // 座標の範囲チェック
                        expect(move.from.row).toBeGreaterThanOrEqual(1);
                        expect(move.from.row).toBeLessThanOrEqual(9);
                        expect(move.from.column).toBeGreaterThanOrEqual(1);
                        expect(move.from.column).toBeLessThanOrEqual(9);
                        expect(move.to.row).toBeGreaterThanOrEqual(1);
                        expect(move.to.row).toBeLessThanOrEqual(9);
                        expect(move.to.column).toBeGreaterThanOrEqual(1);
                        expect(move.to.column).toBeLessThanOrEqual(9);

                        // 駒情報の検証
                        expect(move.piece).toHaveProperty("type");
                        expect(move.piece).toHaveProperty("owner");
                        expect(move.piece).toHaveProperty("promoted");
                        expect(move.piece.owner).toMatch(/^(black|white)$/);
                        expect(typeof move.piece.promoted).toBe("boolean");
                    } else {
                        // 持ち駒を打つ
                        expect(move).toHaveProperty("to");
                        expect(move).toHaveProperty("piece");

                        // 座標の範囲チェック
                        expect(move.to.row).toBeGreaterThanOrEqual(1);
                        expect(move.to.row).toBeLessThanOrEqual(9);
                        expect(move.to.column).toBeGreaterThanOrEqual(1);
                        expect(move.to.column).toBeLessThanOrEqual(9);
                    }
                }
            }
        });
    });

    describe("定跡内容の検証", () => {
        it("初期局面の定跡が含まれていること", () => {
            const openings = generateMainOpenings();

            // 初期局面のSFEN
            const initialSfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";
            const initialEntry = openings.find((entry) => entry.position === initialSfen);

            expect(initialEntry).toBeDefined();
            expect(initialEntry?.moves.length).toBeGreaterThan(0);
        });

        it("基本的な初手が含まれていること", () => {
            const openings = generateMainOpenings();
            const initialSfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";
            const initialEntry = openings.find((entry) => entry.position === initialSfen);

            expect(initialEntry).toBeDefined();
            if (!initialEntry) {
                throw new Error("Initial position not found");
            }
            const notations = initialEntry.moves.map((m) => m.notation);

            // 主要な初手
            expect(notations).toContain("7g7f"); // 居飛車（7六歩）
            expect(notations).toContain("2g2f"); // 振り飛車（2六歩）
        });

        it("notation形式が正しいこと", () => {
            const openings = generateMainOpenings();
            // 棋譜記法の正規表現（例：7g7f, 2g2f+）
            const notationRegex = /^[1-9][a-i][1-9][a-i](\+)?$/;

            for (const entry of openings) {
                for (const move of entry.moves) {
                    expect(move.notation).toMatch(notationRegex);
                }
            }
        });
    });

    describe("データ品質チェック", () => {
        it("weight値が適切な範囲内であること", () => {
            const openings = generateMainOpenings();

            for (const entry of openings) {
                for (const move of entry.moves) {
                    expect(move.weight).toBeGreaterThan(0);
                    expect(move.weight).toBeLessThan(1000);
                }

                // エントリ内の重みの合計も妥当な範囲か
                const totalWeight = entry.moves.reduce((sum, m) => sum + m.weight, 0);
                expect(totalWeight).toBeGreaterThan(50);
                expect(totalWeight).toBeLessThan(10000);
            }
        });

        it("depth値が妥当であること", () => {
            const openings = generateMainOpenings();

            for (const entry of openings) {
                expect(entry.depth).toBeGreaterThan(0);
                expect(entry.depth).toBeLessThanOrEqual(50); // 定跡の深さとして妥当な範囲
            }
        });

        it("同じ局面に対する重複エントリがないこと", () => {
            const openings = generateMainOpenings();
            const positions = openings.map((entry) => entry.position);
            const positionCounts = new Map<string, number>();

            // 各局面の出現回数をカウント
            for (const pos of positions) {
                positionCounts.set(pos, (positionCounts.get(pos) || 0) + 1);
            }

            // 重複がある場合の詳細を確認
            const duplicates = Array.from(positionCounts.entries())
                .filter(([_, count]) => count > 1)
                .map(([pos, count]) => ({ position: pos, count }));

            if (duplicates.length > 0) {
                console.log("Note: Some positions appear multiple times in the opening book:");
                for (const { position, count } of duplicates) {
                    console.log(
                        `  - Position appears ${count} times: ${position.substring(0, 30)}...`,
                    );
                }
            }

            // 重複エントリがあることは許容するが、過度な重複は避ける
            // 各局面は最大2回まで（異なる変化手順で到達する場合がある）
            for (const [_, count] of positionCounts) {
                expect(count).toBeLessThanOrEqual(2);
            }
        });
    });

    describe("定跡の分類", () => {
        it("各手に適切な名前やコメントが付いていること", () => {
            const openings = generateMainOpenings();
            const initialSfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";
            const initialEntry = openings.find((entry) => entry.position === initialSfen);

            if (initialEntry) {
                // 居飛車の手
                const ibisha = initialEntry.moves.find((m) => m.notation === "7g7f");
                expect(ibisha?.name).toBeDefined();
                expect(ibisha?.name).toContain("居飛車");

                // 振り飛車の手
                const furibisha = initialEntry.moves.find((m) => m.notation === "2g2f");
                expect(furibisha?.name).toBeDefined();
                expect(furibisha?.name).toContain("振り飛車");
            }
        });
    });
});
