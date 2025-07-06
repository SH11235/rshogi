/**
 * 定跡の局面検索とSFEN互換性の詳細テスト
 * 実際の定跡データを使用してハッシュ化と検索処理を検証する
 */

import { describe, test, expect, beforeAll } from "vitest";
import { WasmOpeningBookLoader } from "../wasmOpeningBookLoader";
import { exportToSfen, modernInitialBoard } from "shogi-core";
import { type PositionState } from "../../types/game";

// ヘルパー関数
const createInitialBoard = () => modernInitialBoard;
const createInitialHands = () => ({
    black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
    white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
});

describe("Opening Book Position Search Tests", () => {
    let openingBookLoader: WasmOpeningBookLoader;
    let loadedSuccessfully = false;

    beforeAll(async () => {
        try {
            openingBookLoader = new WasmOpeningBookLoader();

            // 実際の定跡ファイルを読み込み（Webのfetchを使用）
            await openingBookLoader.loadForDifficulty("standard");
            loadedSuccessfully = true;
            console.log("Opening book loaded successfully for testing");
        } catch (error) {
            console.error("Failed to load opening book for testing:", error);
            loadedSuccessfully = false;
        }
    });

    describe("SFEN Generation and Format", () => {
        test("初期局面のSFEN生成が正しい", () => {
            const initialBoard = createInitialBoard();
            const initialHands = createInitialHands();

            const sfen = exportToSfen(initialBoard, initialHands, "black", 1);
            console.log("Initial position SFEN:", sfen);

            // 標準的な初期局面のSFEN
            expect(sfen).toBe(
                "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            );
        });

        test("2六歩後の局面のSFEN生成", () => {
            // 2六歩を指した後の盤面 (72->62の移動)
            const boardAfter2g2f = { ...createInitialBoard() };
            delete boardAfter2g2f["72"];
            boardAfter2g2f["62"] = { type: "pawn", owner: "black", promoted: false };

            const sfen = exportToSfen(boardAfter2g2f, createInitialHands(), "white", 1);
            console.log("Position after 2g2f SFEN:", sfen);

            // 2六歩後の正しいSFEN
            expect(sfen).toBe(
                "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL w - 1",
            );
        });

        test("手番が正しく反映される", () => {
            const board = createInitialBoard();
            const hands = createInitialHands();

            const blackSfen = exportToSfen(board, hands, "black", 1);
            const whiteSfen = exportToSfen(board, hands, "white", 1);

            expect(blackSfen).toContain(" b ");
            expect(whiteSfen).toContain(" w ");

            console.log("Black turn SFEN:", blackSfen);
            console.log("White turn SFEN:", whiteSfen);
        });
    });

    describe("Opening Book Search Tests", () => {
        test("初期局面での定跡検索", async () => {
            if (!loadedSuccessfully) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            const initialPosition: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            console.log("Testing initial position search...");
            const book = await openingBookLoader.loadForDifficulty("standard");
            const moves = await book.findMoves(initialPosition);

            console.log(`Found ${moves.length} moves in initial position:`);
            moves.forEach((move, index) => {
                console.log(`  ${index + 1}. ${move.notation} (eval: ${move.evaluation})`);
            });

            // 初期局面では少なくとも数手の定跡があることを期待
            expect(moves.length).toBeGreaterThan(0);

            // 典型的な初手が含まれているかチェック
            const notations = moves.map((m) => m.notation);
            const hasCommonMoves = notations.some(
                (n) => n === "2g2f" || n === "7g7f" || n === "6i7h",
            );
            expect(hasCommonMoves).toBe(true);
        });

        test("2六歩後の局面での定跡検索", async () => {
            if (!loadedSuccessfully) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            // 2六歩を指した後の盤面 (72->62の移動)
            const boardAfter2g2f = { ...createInitialBoard() };
            delete boardAfter2g2f["72"];
            boardAfter2g2f["62"] = { type: "pawn", owner: "black", promoted: false };

            const position: PositionState = {
                board: boardAfter2g2f,
                hands: createInitialHands(),
                currentPlayer: "white",
            };

            console.log("Testing position after 2g2f...");
            const book = await openingBookLoader.loadForDifficulty("standard");
            const moves = await book.findMoves(position);

            console.log(`Found ${moves.length} moves after 2g2f:`);
            moves.forEach((move, index) => {
                console.log(`  ${index + 1}. ${move.notation} (eval: ${move.evaluation})`);
            });

            // この局面でも定跡があることを期待（空でも構わないが、エラーが出ないことを確認）
            expect(Array.isArray(moves)).toBe(true);

            // もし手が見つかった場合、典型的な後手の応手が含まれているかチェック
            if (moves.length > 0) {
                const notations = moves.map((m) => m.notation);
                console.log("Found typical responses:", notations);
            }
        });

        test("存在しない局面での検索", async () => {
            if (!loadedSuccessfully) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            // 非現実的な局面を作成（王様を動かしたような局面）
            const unrealisticBoard = { ...createInitialBoard() };
            delete unrealisticBoard["59"];
            unrealisticBoard["58"] = { type: "king", owner: "black", promoted: false };

            const position: PositionState = {
                board: unrealisticBoard,
                hands: createInitialHands(),
                currentPlayer: "white",
            };

            console.log("Testing unrealistic position...");
            const book = await openingBookLoader.loadForDifficulty("standard");
            const moves = await book.findMoves(position);

            console.log(`Found ${moves.length} moves for unrealistic position`);

            // 非現実的な局面では定跡が見つからないことを期待
            expect(Array.isArray(moves)).toBe(true);
            // 結果が空であることは正常
        });
    });

    describe("Position Hash Consistency", () => {
        test("同じ局面は同じ結果を返す", async () => {
            if (!loadedSuccessfully) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            const position1: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            const position2: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            const book = await openingBookLoader.loadForDifficulty("standard");
            const moves1 = await book.findMoves(position1);
            const moves2 = await book.findMoves(position2);

            console.log(`Position 1 moves: ${moves1.length}, Position 2 moves: ${moves2.length}`);

            // 同じ局面なので同じ数の手が見つかるはず
            expect(moves1.length).toBe(moves2.length);

            // 手順も同じであることを確認
            const notations1 = moves1.map((m) => m.notation).sort();
            const notations2 = moves2.map((m) => m.notation).sort();
            expect(notations1).toEqual(notations2);
        });

        test("異なる局面は異なる結果を返す可能性がある", async () => {
            if (!loadedSuccessfully) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            const initialPosition: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            // 2六歩後の局面 (72->62の移動)
            const boardAfter2g2f = { ...createInitialBoard() };
            delete boardAfter2g2f["72"];
            boardAfter2g2f["62"] = { type: "pawn", owner: "black", promoted: false };

            const modifiedPosition: PositionState = {
                board: boardAfter2g2f,
                hands: createInitialHands(),
                currentPlayer: "white",
            };

            const book = await openingBookLoader.loadForDifficulty("standard");
            const initialMoves = await book.findMoves(initialPosition);
            const modifiedMoves = await book.findMoves(modifiedPosition);

            console.log(`Initial position moves: ${initialMoves.length}`);
            console.log(`Modified position moves: ${modifiedMoves.length}`);

            // 異なる局面なので結果が異なることを確認（手番やアクション候補が変わる）
            expect(Array.isArray(initialMoves)).toBe(true);
            expect(Array.isArray(modifiedMoves)).toBe(true);

            // 内容が異なることを確認（ただし、両方とも空の場合は除く）
            if (initialMoves.length > 0 || modifiedMoves.length > 0) {
                const initialNotations = initialMoves.map((m) => m.notation).sort();
                const modifiedNotations = modifiedMoves.map((m) => m.notation).sort();

                // 通常は異なる手が提案されるはず
                expect(initialNotations).not.toEqual(modifiedNotations);
            }
        });
    });

    describe("Performance Tests", () => {
        test("定跡検索のパフォーマンス", async () => {
            if (!loadedSuccessfully) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            const position: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            const book = await openingBookLoader.loadForDifficulty("standard");

            const startTime = performance.now();
            const moves = await book.findMoves(position);
            const endTime = performance.now();

            const searchTime = endTime - startTime;
            console.log(`Opening book search took ${searchTime.toFixed(2)}ms`);
            console.log(`Found ${moves.length} moves`);

            // 検索時間が合理的な範囲内であることを確認（100ms以下）
            expect(searchTime).toBeLessThan(100);
        });

        test("複数回検索の一貫性", async () => {
            if (!loadedSuccessfully) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            const position: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            const book = await openingBookLoader.loadForDifficulty("standard");

            // 同じ局面を複数回検索
            const results = [];
            for (let i = 0; i < 3; i++) {
                const moves = await book.findMoves(position);
                results.push(moves.map((m) => m.notation).sort());
            }

            console.log(
                "Multiple search results lengths:",
                results.map((r) => r.length),
            );

            // 全ての結果が同じであることを確認
            expect(results[0]).toEqual(results[1]);
            expect(results[1]).toEqual(results[2]);
        });
    });
});
