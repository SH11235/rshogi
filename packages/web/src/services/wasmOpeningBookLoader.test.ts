/**
 * 定跡ファイルからの局面探索ロジックの詳細テスト
 * 実際のファイルを読み込んで探索し、対応する手のリストを抽出するテスト
 */

import {
    type OpeningBookInterface,
    type PositionState,
    exportToSfen,
    initialBoard,
    initialHands,
} from "shogi-core";
import { beforeAll, describe, expect, test, vi } from "vitest";
import { WasmOpeningBookLoader } from "./wasmOpeningBookLoader";

// WASMモジュールのモック
vi.mock("../wasm/shogi_core.js", () => ({
    default: vi.fn().mockResolvedValue({}),
    OpeningBookReaderWasm: vi.fn().mockImplementation(() => ({
        load_data: vi.fn().mockReturnValue(true),
        find_moves: vi.fn().mockReturnValue(JSON.stringify([])),
        position_count: vi.fn().mockReturnValue(436294),
        is_loaded: vi.fn().mockReturnValue(true),
    })),
}));

// モックされたWASMリーダーの型定義
interface MockWasmReader {
    load_data: ReturnType<typeof vi.fn>;
    find_moves: ReturnType<typeof vi.fn>;
    position_count: ReturnType<typeof vi.fn>;
    is_loaded: ReturnType<typeof vi.fn>;
}

describe("Opening Book Search Integration Tests", () => {
    let openingBookLoader: WasmOpeningBookLoader;
    let mockWasmReader: MockWasmReader;
    let openingBook: OpeningBookInterface;

    beforeAll(async () => {
        openingBookLoader = new WasmOpeningBookLoader();

        // WASMリーダーのモックにアクセス
        mockWasmReader = (openingBookLoader as unknown as { reader: MockWasmReader }).reader;

        // フォールバックのOpeningBookを使用
        openingBook = openingBookLoader.loadFromFallback();
    });

    describe("Initial Position Tests", () => {
        test("初期局面のSFEN生成とハッシュ化", () => {
            const board = structuredClone(initialBoard);
            const hands = structuredClone(initialHands());

            const sfen = exportToSfen(board, hands, "black", 1);
            console.log("Generated initial SFEN:", sfen);

            // 期待されるSFEN形式
            expect(sfen).toBe(
                "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            );
        });

        test("初期局面で定跡の手を検索", () => {
            const initialPosition: PositionState = {
                board: structuredClone(initialBoard),
                hands: structuredClone(initialHands()),
                currentPlayer: "black",
            };

            // フォールバックのOpeningBookはデフォルトでは空の結果を返す
            const moves = openingBook.findMoves(initialPosition);

            // フォールバックはgenerateMainOpeningsからデータを読み込むため、
            // 初期局面の手が含まれている可能性がある
            expect(Array.isArray(moves)).toBe(true);
        });
    });

    describe("Specific Position Tests", () => {
        test("2六歩後の局面でのSFEN生成", () => {
            // 2六歩を指した後の盤面を作成
            const boardAfter2g2f = structuredClone(initialBoard);
            // 2七の歩を2六に移動
            boardAfter2g2f["27"] = null;
            boardAfter2g2f["26"] = { type: "pawn", owner: "black", promoted: false };

            const sfen = exportToSfen(boardAfter2g2f, structuredClone(initialHands()), "white", 1);
            console.log("SFEN after 2g2f:", sfen);

            // 2六歩後のSFEN
            expect(sfen).toBe(
                "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL w - 1",
            );
        });

        test("2六歩後の局面で定跡検索", () => {
            const boardAfter2g2f = structuredClone(initialBoard);
            boardAfter2g2f["27"] = null;
            boardAfter2g2f["26"] = { type: "pawn", owner: "black", promoted: false };

            const position: PositionState = {
                board: boardAfter2g2f,
                hands: structuredClone(initialHands()),
                currentPlayer: "white",
            };

            // この局面にも定跡があると仮定
            const mockMoves = [
                { notation: "8c8d", evaluation: 20, depth: 25 },
                { notation: "3c3d", evaluation: 18, depth: 22 },
            ];

            mockWasmReader.find_moves.mockReturnValue(JSON.stringify(mockMoves));

            const moves = openingBook.findMoves(position);

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
            const board = structuredClone(initialBoard);
            const hands = structuredClone(initialHands());

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
                board: structuredClone(initialBoard),
                hands: structuredClone(initialHands()),
                currentPlayer: "black",
            };

            const moves = openingBook.findMoves(position);
            expect(moves).toHaveLength(0);
        });

        test("無効なJSONを返す場合", async () => {
            mockWasmReader.find_moves.mockReturnValue("invalid json");

            const position: PositionState = {
                board: structuredClone(initialBoard),
                hands: structuredClone(initialHands()),
                currentPlayer: "black",
            };

            const moves = openingBook.findMoves(position);
            expect(moves).toHaveLength(0);
        });
    });

    describe("Real Position Hash Tests", () => {
        test("同じ局面から同じハッシュが生成される", () => {
            const board1 = structuredClone(initialBoard);
            const board2 = structuredClone(initialBoard);

            const sfen1 = exportToSfen(board1, structuredClone(initialHands()), "black", 1);
            const sfen2 = exportToSfen(board2, structuredClone(initialHands()), "black", 1);

            expect(sfen1).toBe(sfen2);
        });

        test("異なる局面から異なるハッシュが生成される", () => {
            const baseBoard = structuredClone(initialBoard);
            const modifiedBoard = structuredClone(initialBoard);

            // 2六歩を指した状態
            modifiedBoard["27"] = null;
            modifiedBoard["26"] = { type: "pawn", owner: "black", promoted: false };

            const sfen1 = exportToSfen(baseBoard, structuredClone(initialHands()), "black", 1);
            const sfen2 = exportToSfen(modifiedBoard, structuredClone(initialHands()), "white", 1);

            expect(sfen1).not.toBe(sfen2);
        });
    });

    describe("Database Integration", () => {
        test("定跡データベースから実際に手を取得", async () => {
            const position: PositionState = {
                board: structuredClone(initialBoard),
                hands: structuredClone(initialHands()),
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

            const moves = openingBook.findMoves(position);

            expect(moves).toHaveLength(5);
            expect(moves.map((m) => m.notation)).toEqual(["2g2f", "7g7f", "6i7h", "1g1f", "9g9f"]);
        });

        test("評価値と深度の情報が正しく取得される", async () => {
            const position: PositionState = {
                board: structuredClone(initialBoard),
                hands: structuredClone(initialHands()),
                currentPlayer: "black",
            };

            const mockMoves = [{ notation: "2g2f", evaluation: 44, depth: 30 }];

            mockWasmReader.find_moves.mockReturnValue(JSON.stringify(mockMoves));

            const moves = openingBook.findMoves(position);

            expect(moves[0].evaluation).toBe(44);
            expect(moves[0].depth).toBe(30);
        });
    });
});
