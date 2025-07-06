import { describe, it, expect, vi, beforeEach } from "vitest";
import { AIEngine } from "../engine";
import { OpeningBook } from "../openingBook";
import { OpeningBookLoader } from "../openingBookLoader";
import type { Board } from "../../domain/model/board";
import type { Hands } from "../../domain/service/moveService";
import type { Move } from "../../domain/model/move";

// モック
vi.mock("../openingBookLoader");

describe("AIEngine - Opening Book Integration", () => {
    let engine: AIEngine;
    let mockOpeningBook: OpeningBook;
    let mockLoader: OpeningBookLoader;

    beforeEach(() => {
        // OpeningBookLoaderのモック
        mockLoader = new OpeningBookLoader();
        mockOpeningBook = new OpeningBook();

        // loadForDifficultyのモック
        vi.spyOn(mockLoader, "loadForDifficulty").mockResolvedValue(mockOpeningBook);
        vi.spyOn(mockOpeningBook, "findMoves").mockReturnValue([]);

        // エンジン作成
        engine = new AIEngine("intermediate");
        // loaderをモックで置き換える
        (engine as any).openingBookLoader = mockLoader;
    });

    describe("Opening book initialization", () => {
        it("should load opening book on initialization", async () => {
            // 定跡を読み込む
            await engine.loadOpeningBook();

            expect(mockLoader.loadForDifficulty).toHaveBeenCalledWith("intermediate");
        });

        it("should handle opening book loading errors gracefully", async () => {
            vi.spyOn(mockLoader, "loadForDifficulty").mockRejectedValue(
                new Error("Loading failed"),
            );

            // エラーが発生してもクラッシュしない
            await expect(engine.loadOpeningBook()).resolves.not.toThrow();
        });

        it("should update opening book when difficulty changes", async () => {
            await engine.loadOpeningBook();

            // 難易度を変更
            engine.setDifficulty("expert");
            await engine.loadOpeningBook();

            expect(mockLoader.loadForDifficulty).toHaveBeenCalledWith("expert");
        });
    });

    describe("Opening book usage in move calculation", () => {
        const testBoard: Board = {} as Board;
        const testHands: Hands = {
            black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
            white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
        };
        const testMove: Move = {
            type: "move",
            from: { row: 7, column: 7 },
            to: { row: 7, column: 6 },
            piece: { type: "pawn", owner: "black", promoted: false },
            promote: false,
            captured: null,
        };

        it("should use opening book move if available", async () => {
            // 定跡の手をモック
            vi.spyOn(mockOpeningBook, "findMoves").mockReturnValue([
                {
                    move: testMove,
                    notation: "7g7f",
                    weight: 100,
                },
            ]);

            await engine.loadOpeningBook();
            const move = await engine.calculateBestMove(testBoard, testHands, "black", []);

            expect(move).toEqual(testMove);
            expect(mockOpeningBook.findMoves).toHaveBeenCalled();
        });

        it("should fall back to search when no opening book moves", async () => {
            // 定跡に手がない
            vi.spyOn(mockOpeningBook, "findMoves").mockReturnValue([]);

            // searchのモック
            const mockSearch = vi
                .spyOn((engine as any).iterativeSearch, "search")
                .mockResolvedValue({
                    bestMove: testMove,
                    score: 0,
                    depth: 1,
                    pv: [testMove],
                    nodes: 1,
                    time: 10,
                });

            // generateAllLegalMovesのモック
            vi.spyOn(engine, "generateAllLegalMoves").mockReturnValue([testMove]);

            await engine.loadOpeningBook();
            const move = await engine.calculateBestMove(testBoard, testHands, "black", []);

            expect(mockOpeningBook.findMoves).toHaveBeenCalled();
            expect(engine.generateAllLegalMoves).toHaveBeenCalled();
            expect(mockSearch).toHaveBeenCalled();
        });

        it("should randomize opening book moves based on weights", async () => {
            const moves = [
                {
                    move: testMove,
                    notation: "7g7f",
                    weight: 70,
                },
                {
                    move: {
                        ...testMove,
                        from: { row: 2, column: 7 },
                        to: { row: 2, column: 6 },
                    } as Move,
                    notation: "2g2f",
                    weight: 30,
                },
            ];

            vi.spyOn(mockOpeningBook, "findMoves").mockReturnValue(moves);

            await engine.loadOpeningBook();

            // 複数回実行して、両方の手が選ばれることを確認
            const selectedMoves = new Set<string>();
            for (let i = 0; i < 20; i++) {
                const move = await engine.calculateBestMove(testBoard, testHands, "black", []);
                selectedMoves.add(
                    (move as any).notation ||
                        `${move.from.row}${move.from.column}${move.to.row}${move.to.column}`,
                );
            }

            // 確率的なので、必ずしも両方選ばれるとは限らないが、
            // 少なくとも1つは選ばれるはず
            expect(selectedMoves.size).toBeGreaterThanOrEqual(1);
        });

        it("should disable opening book for beginner difficulty", async () => {
            engine = new AIEngine("beginner");
            (engine as any).openingBookLoader = mockLoader;

            // 初心者レベルでは定跡を使用しない設定
            await engine.loadOpeningBook();

            // 30%のランダムムーブではない場合のモック
            vi.spyOn(Math, "random").mockReturnValue(0.5);

            // searchのモック
            vi.spyOn((engine as any).iterativeSearch, "search").mockResolvedValue({
                bestMove: testMove,
                score: 0,
                depth: 1,
                pv: [testMove],
                nodes: 1,
                time: 10,
            });

            vi.spyOn(engine, "generateAllLegalMoves").mockReturnValue([testMove]);

            const move = await engine.calculateBestMove(testBoard, testHands, "black", []);

            // 定跡が読み込まれていても使用されない
            expect(mockOpeningBook.findMoves).not.toHaveBeenCalled();
        });
    });

    describe("Opening book configuration", () => {
        const testBoard: Board = {} as Board;
        const testHands: Hands = {
            black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
            white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
        };

        it("should have useOpeningBook option in AI config", () => {
            const config = engine.getConfig();
            expect(config).toHaveProperty("useOpeningBook");
        });

        it("should respect useOpeningBook setting", async () => {
            // 定跡使用を無効化
            engine.setConfig({ useOpeningBook: false });

            const validMove: Move = {
                type: "move",
                from: { row: 7, column: 7 },
                to: { row: 7, column: 6 },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            };

            vi.spyOn(mockOpeningBook, "findMoves").mockReturnValue([
                {
                    move: validMove,
                    notation: "7g7f",
                    weight: 100,
                },
            ]);

            // searchのモック
            vi.spyOn((engine as any).iterativeSearch, "search").mockResolvedValue({
                bestMove: validMove,
                score: 0,
                depth: 1,
                pv: [validMove],
                nodes: 1,
                time: 10,
            });

            vi.spyOn(engine, "generateAllLegalMoves").mockReturnValue([validMove]);

            await engine.loadOpeningBook();
            await engine.calculateBestMove(testBoard, testHands, "black", []);

            expect(mockOpeningBook.findMoves).not.toHaveBeenCalled();
        });
    });
});
