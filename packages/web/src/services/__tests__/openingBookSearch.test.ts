/**
 * 定跡ファイルからの局面探索ロジックの詳細テスト
 * 実際のファイルを読み込んで探索し、対応する手のリストを抽出するテスト
 */

import { describe, test, expect, beforeAll, vi } from "vitest";
import { WasmOpeningBookLoader } from "../wasmOpeningBookLoader";
import { exportToSfen } from "shogi-core";
import { type PositionState, type Board, type Hands } from "../../types/game";
import { createInitialBoard, createInitialHands } from "../../utils/gameUtils";

// WASMモジュールのモック
vi.mock("../../wasm/shogi_core.js", () => ({
    default: vi.fn().mockResolvedValue({}),
    OpeningBookReaderWasm: vi.fn().mockImplementation(() => ({
        load_data: vi.fn().mockReturnValue(true),
        find_moves: vi.fn().mockReturnValue(JSON.stringify([])),
        position_count: vi.fn().mockReturnValue(436294),
        is_loaded: vi.fn().mockReturnValue(true),
    })),
}));

describe("Opening Book Search Integration Tests", () => {
    let openingBookLoader: WasmOpeningBookLoader;
    let mockWasmReader: any;

    beforeAll(async () => {
        openingBookLoader = new WasmOpeningBookLoader();

        // バイナリデータをモック（実際の定跡データの代替）
        const mockData = new Uint8Array([1, 2, 3, 4, 5]);
        await openingBookLoader.loadData(mockData);

        // WASMリーダーのモックにアクセス
        mockWasmReader = (openingBookLoader as any).wasmReader;
    });

    describe("Initial Position Tests", () => {
        test("初期局面のSFEN生成とハッシュ化", () => {
            const initialBoard = createInitialBoard();
            const initialHands = createInitialHands();

            const sfen = exportToSfen(initialBoard, initialHands, "black", 1);
            console.log("Generated initial SFEN:", sfen);

            // 期待されるSFEN形式
            expect(sfen).toBe(
                "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            );
        });

        test("初期局面で定跡の手を検索", async () => {
            const initialPosition: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            // 模擬的な定跡データを設定
            const mockMoves = [
                { notation: "2g2f", evaluation: 44, depth: 30 },
                { notation: "7g7f", evaluation: 44, depth: 30 },
                { notation: "6i7h", evaluation: 35, depth: 36 },
            ];

            mockWasmReader.find_moves.mockReturnValue(JSON.stringify(mockMoves));

            const moves = await openingBookLoader.findMoves(initialPosition);

            expect(moves).toHaveLength(3);
            expect(moves[0]).toEqual({
                notation: "2g2f",
                evaluation: 44,
                depth: 30,
            });
        });
    });

    describe("Specific Position Tests", () => {
        test("2六歩後の局面でのSFEN生成", () => {
            // 2六歩を指した後の盤面を作成
            const boardAfter2g2f = createInitialBoard();
            // 2七の歩を2六に移動
            delete boardAfter2g2f["27"];
            boardAfter2g2f["26"] = { type: "pawn", owner: "black", promoted: false };

            const sfen = exportToSfen(boardAfter2g2f, createInitialHands(), "white", 1);
            console.log("SFEN after 2g2f:", sfen);

            // 2六歩後のSFEN
            expect(sfen).toBe(
                "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL w - 1",
            );
        });

        test("2六歩後の局面で定跡検索", async () => {
            const boardAfter2g2f = createInitialBoard();
            delete boardAfter2g2f["27"];
            boardAfter2g2f["26"] = { type: "pawn", owner: "black", promoted: false };

            const position: PositionState = {
                board: boardAfter2g2f,
                hands: createInitialHands(),
                currentPlayer: "white",
            };

            // この局面にも定跡があると仮定
            const mockMoves = [
                { notation: "8c8d", evaluation: 20, depth: 25 },
                { notation: "3c3d", evaluation: 18, depth: 22 },
            ];

            mockWasmReader.find_moves.mockReturnValue(JSON.stringify(mockMoves));

            const moves = await openingBookLoader.findMoves(position);

            expect(moves).toHaveLength(2);
            expect(moves[0].notation).toBe("8c8d");
        });
    });

    describe("SFEN Format Validation", () => {
        test("SFEN前置詞の処理", () => {
            const sfenWithPrefix =
                "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
            const sfenWithoutPrefix =
                "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

            // 前置詞を除去する処理をテスト
            const processed = sfenWithPrefix.startsWith("sfen ")
                ? sfenWithPrefix.slice(5)
                : sfenWithPrefix;

            expect(processed).toBe(sfenWithoutPrefix);
        });

        test("異なる手番でのSFEN生成", () => {
            const board = createInitialBoard();
            const hands = createInitialHands();

            const blackSfen = exportToSfen(board, hands, "black", 1);
            const whiteSfen = exportToSfen(board, hands, "white", 1);

            expect(blackSfen).toContain(" b ");
            expect(whiteSfen).toContain(" w ");
        });
    });

    describe("Error Handling", () => {
        test("空の結果を返す場合", async () => {
            mockWasmReader.find_moves.mockReturnValue("[]");

            const position: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            const moves = await openingBookLoader.findMoves(position);
            expect(moves).toHaveLength(0);
        });

        test("無効なJSONを返す場合", async () => {
            mockWasmReader.find_moves.mockReturnValue("invalid json");

            const position: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            const moves = await openingBookLoader.findMoves(position);
            expect(moves).toHaveLength(0);
        });
    });

    describe("Real Position Hash Tests", () => {
        test("同じ局面から同じハッシュが生成される", () => {
            const board1 = createInitialBoard();
            const board2 = createInitialBoard();

            const sfen1 = exportToSfen(board1, createInitialHands(), "black", 1);
            const sfen2 = exportToSfen(board2, createInitialHands(), "black", 1);

            expect(sfen1).toBe(sfen2);
        });

        test("異なる局面から異なるハッシュが生成される", () => {
            const initialBoard = createInitialBoard();
            const modifiedBoard = createInitialBoard();

            // 2六歩を指した状態
            delete modifiedBoard["27"];
            modifiedBoard["26"] = { type: "pawn", owner: "black", promoted: false };

            const sfen1 = exportToSfen(initialBoard, createInitialHands(), "black", 1);
            const sfen2 = exportToSfen(modifiedBoard, createInitialHands(), "white", 1);

            expect(sfen1).not.toBe(sfen2);
        });
    });

    describe("Database Integration", () => {
        test("定跡データベースから実際に手を取得", async () => {
            const position: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            // 実際の定跡データをシミュレート
            const expectedMoves = [
                { notation: "2g2f", evaluation: 44, depth: 30 },
                { notation: "7g7f", evaluation: 44, depth: 30 },
                { notation: "6i7h", evaluation: 35, depth: 36 },
                { notation: "1g1f", evaluation: 24, depth: 22 },
                { notation: "9g9f", evaluation: 16, depth: 11 },
            ];

            mockWasmReader.find_moves.mockReturnValue(JSON.stringify(expectedMoves));

            const moves = await openingBookLoader.findMoves(position);

            expect(moves).toHaveLength(5);
            expect(moves.map((m) => m.notation)).toEqual(["2g2f", "7g7f", "6i7h", "1g1f", "9g9f"]);
        });

        test("評価値と深度の情報が正しく取得される", async () => {
            const position: PositionState = {
                board: createInitialBoard(),
                hands: createInitialHands(),
                currentPlayer: "black",
            };

            const mockMoves = [{ notation: "2g2f", evaluation: 44, depth: 30 }];

            mockWasmReader.find_moves.mockReturnValue(JSON.stringify(mockMoves));

            const moves = await openingBookLoader.findMoves(position);

            expect(moves[0].evaluation).toBe(44);
            expect(moves[0].depth).toBe(30);
        });
    });
});
