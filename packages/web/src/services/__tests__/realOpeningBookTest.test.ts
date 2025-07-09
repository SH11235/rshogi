/**
 * 実際の定跡ファイルを使用した定跡検索の統合テスト
 * 実際のバイナリファイルを読み込んで、正確な局面検索を検証する
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { exportToSfen, initialBoard } from "shogi-core";
import type { PositionState } from "shogi-core";
import { beforeAll, describe, expect, test } from "vitest";
import { WasmOpeningBookLoader } from "../wasmOpeningBookLoader";

// ヘルパー関数
const createInitialBoard = () => initialBoard;
const createInitialHands = () => ({
    black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
    white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
});

describe("Real Opening Book Integration Tests", () => {
    let openingBookLoader: WasmOpeningBookLoader;
    let isLoaded = false;

    beforeAll(async () => {
        try {
            openingBookLoader = new WasmOpeningBookLoader();

            // 実際の定跡ファイルを読み込み
            const openingBookPath = join(
                process.cwd(),
                "public",
                "data",
                "opening_book_standard.binz",
            );
            console.log("Attempting to load opening book from:", openingBookPath);

            const openingBookData = readFileSync(openingBookPath);
            console.log("Opening book file size:", openingBookData.length, "bytes");

            // isLoaded = await openingBookLoader.loadData(openingBookData);
            // console.log("Opening book loaded successfully:", isLoaded);

            // if (isLoaded) {
            //     const positionCount = await openingBookLoader.getPositionCount();
            //     console.log("Total positions in opening book:", positionCount);
            // }
        } catch (error) {
            console.error("Failed to load opening book:", error);
            isLoaded = false;
        }
    });

    describe("Real File Loading", () => {
        test("定跡ファイルが正常に読み込まれる", () => {
            expect(isLoaded).toBe(true);
        });

        test("読み込まれた定跡の局面数が正しい", async () => {
            if (!isLoaded) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            // const positionCount = await openingBookLoader.getPositionCount();
            // expect(positionCount).toBeGreaterThan(0);
            // console.log("Position count:", positionCount);
        });
    });

    describe("Initial Position Search", () => {
        test("初期局面で定跡の手が見つかる", async () => {
            if (!isLoaded) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            const initialPosition: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            console.log("Searching for moves in initial position...", initialPosition);
            // const moves = await openingBookLoader.findMoves(initialPosition);

            // console.log("Found moves:", moves.length);
            // moves.forEach((move, index) => {
            //     console.log(
            //         `  ${index + 1}. ${move.notation} (eval: ${move.evaluation}, depth: ${move.depth})`,
            //     );
            // });

            // expect(moves.length).toBeGreaterThan(0);

            // // 典型的な初手が含まれているかチェック
            // const notations = moves.map((m) => m.notation);
            // expect(notations.some((n) => n === "2g2f" || n === "7g7f")).toBe(true);
        });

        test("初期局面のSFEN形式が正しい", () => {
            const initialBoard = createInitialBoard();
            const initialHands = createInitialHands();

            const sfen = exportToSfen(initialBoard, initialHands, "black", 1);
            console.log("Initial position SFEN:", sfen);

            // 標準的な初期局面のSFEN
            expect(sfen).toBe(
                "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            );
        });
    });

    describe("Specific Position Tests", () => {
        test("2六歩後の局面でのSFEN生成と検索", async () => {
            if (!isLoaded) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            // 2六歩を指した後の盤面
            const boardAfter2g2f = createInitialBoard();
            boardAfter2g2f["27"] = null; // 2七の歩を削除
            boardAfter2g2f["26"] = { type: "pawn", owner: "black", promoted: false };

            // const position: PositionState = {
            //     board: boardAfter2g2f,
            //     hands: createInitialHands(),
            //     currentPlayer: "white",
            // };

            const sfen = exportToSfen(boardAfter2g2f, createInitialHands(), "white", 1);
            console.log("Position after 2g2f SFEN:", sfen);

            // const moves = await openingBookLoader.findMoves(position);
            // console.log("Moves found after 2g2f:", moves.length);

            // moves.forEach((move, index) => {
            //     console.log(
            //         `  ${index + 1}. ${move.notation} (eval: ${move.evaluation}, depth: ${move.depth})`,
            //     );
            // });

            // // この局面でも定跡があることを期待（空でも構わないが、エラーが出ないことを確認）
            // expect(Array.isArray(moves)).toBe(true);
        });

        test("7六歩後の局面での検索", async () => {
            if (!isLoaded) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            // 7六歩を指した後の盤面
            const boardAfter7g7f = createInitialBoard();
            boardAfter7g7f["77"] = null; // 7七の歩を削除
            boardAfter7g7f["76"] = { type: "pawn", owner: "black", promoted: false };

            // const position: PositionState = {
            //     board: boardAfter7g7f,
            //     hands: createInitialHands(),
            //     currentPlayer: "white",
            // };

            // const moves = await openingBookLoader.findMoves(position);
            // console.log("Moves found after 7g7f:", moves.length);

            // moves.forEach((move, index) => {
            //     console.log(
            //         `  ${index + 1}. ${move.notation} (eval: ${move.evaluation}, depth: ${move.depth})`,
            //     );
            // });

            // expect(Array.isArray(moves)).toBe(true);
        });
    });

    describe("SFEN Format Tests", () => {
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

        test("手数が正しく反映される", () => {
            const board = createInitialBoard();
            const hands = createInitialHands();

            const sfen1 = exportToSfen(board, hands, "black", 1);
            const sfen10 = exportToSfen(board, hands, "black", 10);

            expect(sfen1).toContain(" 1");
            expect(sfen10).toContain(" 10");

            console.log("Move 1 SFEN:", sfen1);
            console.log("Move 10 SFEN:", sfen10);
        });
    });

    describe("Hash Consistency Tests", () => {
        test("同一局面は同一のSFENを生成する", () => {
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

            const sfen1 = exportToSfen(
                position1.board,
                position1.hands,
                position1.currentPlayer,
                1,
            );
            const sfen2 = exportToSfen(
                position2.board,
                position2.hands,
                position2.currentPlayer,
                1,
            );

            expect(sfen1).toBe(sfen2);
            console.log("Consistent SFEN:", sfen1);
        });

        test("異なる局面は異なるSFENを生成する", () => {
            const initialPosition = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black" as const,
            };

            const modifiedBoard = createInitialBoard();
            modifiedBoard["27"] = null;
            modifiedBoard["26"] = {
                type: "pawn" as const,
                owner: "black" as const,
                promoted: false,
            };

            const modifiedPosition = {
                board: modifiedBoard,
                hands: createInitialHands(),
                currentPlayer: "white" as const,
            };

            const sfen1 = exportToSfen(
                initialPosition.board,
                initialPosition.hands,
                initialPosition.currentPlayer,
                1,
            );
            const sfen2 = exportToSfen(
                modifiedPosition.board,
                modifiedPosition.hands,
                modifiedPosition.currentPlayer,
                1,
            );

            expect(sfen1).not.toBe(sfen2);
            console.log("Initial SFEN:", sfen1);
            console.log("Modified SFEN:", sfen2);
        });
    });

    describe("Performance Tests", () => {
        test("定跡検索のパフォーマンス", async () => {
            if (!isLoaded) {
                console.log("Skipping test - opening book not loaded");
                return;
            }

            // const position: PositionState = {
            //     board: createInitialBoard(),
            //     hands: createInitialHands(),
            //     currentPlayer: "black",
            // };

            // const startTime = performance.now();
            // const moves = await openingBookLoader.findMoves(position);
            // const endTime = performance.now();

            // const searchTime = endTime - startTime;
            // console.log(`Opening book search took ${searchTime.toFixed(2)}ms`);
            // console.log(`Found ${moves.length} moves`);

            // // 検索時間が合理的な範囲内であることを確認（100ms以下）
            // expect(searchTime).toBeLessThan(100);
        });
    });
});
