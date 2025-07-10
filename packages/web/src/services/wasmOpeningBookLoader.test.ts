import type { Board, Move, OpeningMove } from "shogi-core";
import { type Mock, beforeEach, describe, expect, it, vi } from "vitest";
import {
    type WasmManager,
    convertWasmMoveToOpeningMove,
    createWasmOpeningBookLoader,
    fetchOpeningBookData,
    parseNotation,
    positionToSfen,
    selectWeightedRandom,
    squareToKey,
} from "./wasmOpeningBookLoader";

// Helper to create a mock WasmManager instance
const createMockWasmManager = (): WasmManager => ({
    initialize: vi.fn().mockResolvedValue(undefined),
});

// モックの型定義
interface MockOpeningBookReaderWasm {
    find_moves: Mock;
    load_data: Mock;
    position_count: number;
}

interface MockWasmManager {
    initialize: Mock<() => Promise<void>>;
}

// exportToSfenのモック
vi.mock("shogi-core", async () => {
    const actual = await vi.importActual<typeof import("shogi-core")>("shogi-core");
    return {
        ...actual,
        exportToSfen: vi.fn(
            (_board, _hands, currentPlayer, moveNumber) =>
                `mock-sfen-${currentPlayer}-${moveNumber}`,
        ),
    };
});

// WASMモジュールのモック
let mockPositionCount = 0;
vi.mock("@/wasm/shogi_core", () => ({
    default: vi.fn(() => Promise.resolve()),
    OpeningBookReaderWasm: vi.fn(() => ({
        find_moves: vi.fn(),
        load_data: vi.fn(() => {
            mockPositionCount = 100; // load_dataが呼ばれたら100に設定
            return true;
        }),
        get position_count() {
            return mockPositionCount;
        },
    })),
}));

// fetchのモック
global.fetch = vi.fn();

describe("wasmOpeningBookLoader", () => {
    beforeEach(() => {
        vi.clearAllMocks();
        mockPositionCount = 0; // 各テストでリセット
    });

    describe("squareToKey", () => {
        it("should convert row and column to board key", () => {
            expect(squareToKey(1, 1)).toBe("11");
            expect(squareToKey(5, 7)).toBe("75");
            expect(squareToKey(9, 9)).toBe("99");
        });
    });

    describe("parseNotation", () => {
        it("should parse valid notation without promotion", () => {
            const result = parseNotation("7g7f");
            expect(result).toEqual({
                fromCol: 7,
                fromRow: 7,
                toCol: 7,
                toRow: 6,
                promote: false,
            });
        });

        it("should parse valid notation with promotion", () => {
            const result = parseNotation("2h2g+");
            expect(result).toEqual({
                fromCol: 2,
                fromRow: 8,
                toCol: 2,
                toRow: 7,
                promote: true,
            });
        });

        it("should return null for invalid notation", () => {
            expect(parseNotation("")).toBeNull();
            expect(parseNotation("7g")).toBeNull();
            expect(parseNotation("invalid")).toBeNull();
            expect(parseNotation("0a1b")).toBeNull(); // out of range
            expect(parseNotation("7g7j")).toBeNull(); // row 'j' is out of range
        });

        it("should handle edge cases", () => {
            // 1a1b (最小値)
            expect(parseNotation("1a1b")).toEqual({
                fromCol: 1,
                fromRow: 1,
                toCol: 1,
                toRow: 2,
                promote: false,
            });

            // 9i9h (最大値)
            expect(parseNotation("9i9h")).toEqual({
                fromCol: 9,
                fromRow: 9,
                toCol: 9,
                toRow: 8,
                promote: false,
            });
        });
    });

    describe("convertWasmMoveToOpeningMove", () => {
        const createBoard = (): Board => {
            const board: Partial<Board> = {};
            // 7七に歩を配置
            board["77"] = { type: "pawn", owner: "black", promoted: false };
            // 2二に飛車を配置
            board["22"] = { type: "rook", owner: "white", promoted: false };
            return board as Board;
        };

        const mockLogger = {
            debug: vi.fn(),
            info: vi.fn(),
            warn: vi.fn(),
            error: vi.fn(),
        };

        it("should convert valid WASM move to OpeningMove", () => {
            const wasmMove = {
                notation: "7g7f",
                evaluation: 42,
                depth: 10,
            };
            const board = createBoard();

            const result = convertWasmMoveToOpeningMove(wasmMove, board, mockLogger);

            expect(result).toEqual({
                move: {
                    type: "move",
                    from: { row: 7, column: 7 },
                    to: { row: 6, column: 7 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                notation: "7g7f",
                weight: 42,
                depth: 10,
            });
        });

        it("should return null for invalid notation", () => {
            const wasmMove = {
                notation: "invalid",
                evaluation: 0,
                depth: 0,
            };
            const board = createBoard();

            const result = convertWasmMoveToOpeningMove(wasmMove, board, mockLogger);

            expect(result).toBeNull();
            expect(mockLogger.warn).toHaveBeenCalledWith("Failed to parse notation: invalid");
        });

        it("should return null when piece not found", () => {
            const wasmMove = {
                notation: "1a1b", // 1一に駒がない
                evaluation: 0,
                depth: 0,
            };
            const board = createBoard();

            const result = convertWasmMoveToOpeningMove(wasmMove, board, mockLogger);

            expect(result).toBeNull();
            expect(mockLogger.warn).toHaveBeenCalledWith("No piece found at 11");
        });

        it("should handle captures correctly", () => {
            const wasmMove = {
                notation: "7g2b", // 7七から2二へ（実際にはありえない動きだがテスト用）
                evaluation: 100,
                depth: 5,
            };
            const board = createBoard();

            const result = convertWasmMoveToOpeningMove(wasmMove, board, mockLogger);

            expect(result).toBeDefined();
            if (result?.move.type === "move") {
                expect(result.move.captured).toEqual({
                    type: "rook",
                    owner: "white",
                    promoted: false,
                });
            }
        });

        it("should use default weight when evaluation is missing", () => {
            const wasmMove = {
                notation: "7g7f",
                evaluation: 0,
                depth: 1,
            };
            const board = createBoard();

            const result = convertWasmMoveToOpeningMove(wasmMove, board, mockLogger);

            expect(result?.weight).toBe(1); // デフォルト値
        });
    });

    describe("positionToSfen", () => {
        it("should convert position to SFEN", () => {
            const position = {
                board: {} as Board,
                hands: { black: {}, white: {} },
                currentPlayer: "black" as const,
            };

            const result = positionToSfen(position, 10);

            expect(result).toBe("mock-sfen-black-10");
        });
    });

    describe("fetchOpeningBookData", () => {
        const mockLogger = {
            debug: vi.fn(),
            info: vi.fn(),
            warn: vi.fn(),
            error: vi.fn(),
        };

        beforeEach(() => {
            (global.fetch as Mock).mockReset();
        });

        it("should fetch and return data successfully", async () => {
            const mockData = new Uint8Array([1, 2, 3, 4, 5]);
            const mockResponse = {
                ok: true,
                arrayBuffer: vi.fn().mockResolvedValue(mockData.buffer),
            };
            (global.fetch as Mock).mockResolvedValue(mockResponse);

            const result = await fetchOpeningBookData("/test/file.binz", mockLogger);

            expect(result).toEqual(mockData);
            expect(global.fetch).toHaveBeenCalledWith("/test/file.binz");
            expect(mockLogger.info).toHaveBeenCalledWith("Fetching file: /test/file.binz");
            expect(mockLogger.info).toHaveBeenCalledWith("Fetched 5 bytes from /test/file.binz");
        });

        it("should throw error when fetch fails", async () => {
            const mockResponse = {
                ok: false,
                status: 404,
                statusText: "Not Found",
            };
            (global.fetch as Mock).mockResolvedValue(mockResponse);

            await expect(fetchOpeningBookData("/test/missing.binz", mockLogger)).rejects.toThrow(
                "Failed to fetch file: 404 Not Found",
            );
        });
    });

    describe("selectWeightedRandom", () => {
        it("should select moves based on weight", () => {
            const moves: OpeningMove[] = [
                { move: { type: "move" } as Move, notation: "7g7f", weight: 10, depth: 1 },
                { move: { type: "move" } as Move, notation: "2g2f", weight: 30, depth: 1 },
                { move: { type: "move" } as Move, notation: "1g1f", weight: 60, depth: 1 },
            ];

            // Math.randomをモック
            const randomSpy = vi.spyOn(Math, "random");

            // weight 10の範囲 (0-0.1)
            randomSpy.mockReturnValueOnce(0.05);
            expect(selectWeightedRandom(moves).notation).toBe("7g7f");

            // weight 30の範囲 (0.1-0.4)
            randomSpy.mockReturnValueOnce(0.25);
            expect(selectWeightedRandom(moves).notation).toBe("2g2f");

            // weight 60の範囲 (0.4-1.0)
            randomSpy.mockReturnValueOnce(0.7);
            expect(selectWeightedRandom(moves).notation).toBe("1g1f");

            randomSpy.mockRestore();
        });

        it("should return first move as fallback", () => {
            const moves: OpeningMove[] = [
                { move: { type: "move" } as Move, notation: "7g7f", weight: 1, depth: 1 },
            ];

            const result = selectWeightedRandom(moves);
            expect(result.notation).toBe("7g7f");
        });
    });

    describe("createWasmOpeningBookLoader", () => {
        let mockWasmManager: MockWasmManager;
        let mockLogger: {
            debug: Mock;
            info: Mock;
            warn: Mock;
            error: Mock;
        };

        beforeEach(() => {
            mockWasmManager = {
                initialize: vi.fn().mockResolvedValue(undefined),
            };
            mockLogger = {
                debug: vi.fn(),
                info: vi.fn(),
                warn: vi.fn(),
                error: vi.fn(),
            };

            // fetch mock setup
            const mockData = new Uint8Array([1, 2, 3]);
            (global.fetch as Mock).mockReset();
            (global.fetch as Mock).mockResolvedValue({
                ok: true,
                arrayBuffer: vi.fn().mockResolvedValue(mockData.buffer),
            });
        });

        it("should create loader and load file", async () => {
            const loader = createWasmOpeningBookLoader({
                logger: mockLogger,
                wasmManager: mockWasmManager,
            });

            const openingBook = await loader.load("/test/opening.binz");

            expect(mockWasmManager.initialize).toHaveBeenCalled();
            expect(mockLogger.info).toHaveBeenCalledWith(
                "Loading opening book from: /test/opening.binz",
            );
            expect(mockLogger.info).toHaveBeenCalledWith(
                "Created new reader for: /test/opening.binz",
            );
            expect(openingBook).toBeDefined();
            expect(openingBook.size()).toBe(100); // モックの値
        });

        it("should not reload already loaded files", async () => {
            const loader = createWasmOpeningBookLoader({
                logger: mockLogger,
                wasmManager: mockWasmManager,
            });

            await loader.load("/test/opening.binz");
            await loader.load("/test/opening.binz"); // 2回目

            expect(global.fetch).toHaveBeenCalledTimes(1); // 1回だけ
            expect(mockLogger.info).toHaveBeenCalledWith(
                "Reusing existing reader for: /test/opening.binz",
            );
            expect(mockLogger.info).toHaveBeenCalledWith(
                "File already loaded: /test/opening.binz, positions: 100",
            );
        });

        it("should load correct file for difficulty", async () => {
            const loader = createWasmOpeningBookLoader({
                logger: mockLogger,
                wasmManager: mockWasmManager,
            });

            await loader.loadForDifficulty("intermediate");

            expect(mockLogger.info).toHaveBeenCalledWith(
                expect.stringContaining("data/opening_book_early.binz"),
            );
        });

        it("should handle errors gracefully", async () => {
            (global.fetch as Mock).mockRejectedValue(new Error("Network error"));

            const loader = createWasmOpeningBookLoader({
                logger: mockLogger,
                wasmManager: mockWasmManager,
            });

            await expect(loader.load("/test/fail.binz")).rejects.toThrow(
                "Failed to load opening book: Network error",
            );
        });
    });

    describe("OpeningBook implementation", () => {
        let mockReader: MockOpeningBookReaderWasm;
        let mockLogger: {
            debug: Mock;
            info: Mock;
            warn: Mock;
            error: Mock;
        };

        beforeEach(() => {
            mockReader = {
                find_moves: vi.fn(),
                load_data: vi.fn(),
                position_count: 100,
            };
            mockLogger = {
                debug: vi.fn(),
                info: vi.fn(),
                warn: vi.fn(),
                error: vi.fn(),
            };

            // Setup fetch mock for each test
            const mockData = new Uint8Array([1, 2, 3]);
            (global.fetch as Mock).mockResolvedValue({
                ok: true,
                arrayBuffer: vi.fn().mockResolvedValue(mockData.buffer),
            });
        });

        it("should find moves from position", async () => {
            const wasmMoves = [
                { notation: "7g7f", evaluation: 10, depth: 5 },
                { notation: "2g2f", evaluation: 20, depth: 5 },
            ];
            mockReader.find_moves.mockReturnValue(JSON.stringify(wasmMoves));

            // OpeningBookReaderWasmのモックを先に設定
            const { OpeningBookReaderWasm } = await import("@/wasm/shogi_core");
            (OpeningBookReaderWasm as Mock).mockImplementation(() => mockReader);

            const loader = createWasmOpeningBookLoader({
                logger: mockLogger,
                wasmManager: createMockWasmManager(),
            });

            const openingBook = await loader.load("/test/opening.binz");

            const board: Board = {} as Board;
            board["77"] = { type: "pawn", owner: "black", promoted: false };
            board["27"] = { type: "pawn", owner: "black", promoted: false };

            const position = {
                board,
                hands: { black: {}, white: {} },
                currentPlayer: "black" as const,
            };

            const moves = openingBook.findMoves(position, { randomize: false });

            expect(moves).toHaveLength(2);
            expect(moves[0].notation).toBe("7g7f");
            expect(moves[1].notation).toBe("2g2f");
        });

        it("should handle randomize option", async () => {
            const wasmMoves = [
                { notation: "7g7f", evaluation: 10, depth: 5 },
                { notation: "2g2f", evaluation: 90, depth: 5 }, // 高い重み
            ];
            mockReader.find_moves.mockReturnValue(JSON.stringify(wasmMoves));

            // Math.randomをモック
            vi.spyOn(Math, "random").mockReturnValue(0.5); // 2番目の手が選ばれる

            // OpeningBookReaderWasmのモックを先に設定
            const { OpeningBookReaderWasm } = await import("@/wasm/shogi_core");
            (OpeningBookReaderWasm as Mock).mockImplementation(() => mockReader);

            const loader = createWasmOpeningBookLoader({
                logger: mockLogger,
                wasmManager: createMockWasmManager(),
            });

            const openingBook = await loader.load("/test/opening.binz");

            const board: Board = {} as Board;
            board["77"] = { type: "pawn", owner: "black", promoted: false };
            board["27"] = { type: "pawn", owner: "black", promoted: false };

            const position = {
                board,
                hands: { black: {}, white: {} },
                currentPlayer: "black" as const,
            };

            const moves = openingBook.findMoves(position, { randomize: true });

            expect(moves).toHaveLength(1);
            expect(moves[0].notation).toBe("2g2f"); // 重みの高い方が選ばれる

            vi.restoreAllMocks();
        });

        it("should return empty array when no moves found", async () => {
            mockReader.find_moves.mockReturnValue("[]");

            // OpeningBookReaderWasmのモックを先に設定
            const { OpeningBookReaderWasm } = await import("@/wasm/shogi_core");
            (OpeningBookReaderWasm as Mock).mockImplementation(() => mockReader);

            const loader = createWasmOpeningBookLoader({
                logger: mockLogger,
                wasmManager: createMockWasmManager(),
            });

            const openingBook = await loader.load("/test/opening.binz");

            const position = {
                board: {} as Board,
                hands: { black: {}, white: {} },
                currentPlayer: "black" as const,
            };

            const moves = openingBook.findMoves(position, {});

            expect(moves).toEqual([]);
        });

        it("should not support addEntry", async () => {
            // OpeningBookReaderWasmのモックを先に設定
            const { OpeningBookReaderWasm } = await import("@/wasm/shogi_core");
            (OpeningBookReaderWasm as Mock).mockImplementation(() => mockReader);

            const loader = createWasmOpeningBookLoader({
                logger: mockLogger,
                wasmManager: createMockWasmManager(),
            });

            const openingBook = await loader.load("/test/opening.binz");

            const mockEntry = {
                position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b", // SFEN format
                moves: [{ move: { type: "move" } as Move, notation: "7g7f", weight: 1, depth: 1 }],
                depth: 1,
            };
            const result = openingBook.addEntry(mockEntry);

            expect(result).toBe(false);
            expect(mockLogger.warn).toHaveBeenCalledWith(
                "addEntry is not supported in WASM implementation",
            );
        });
    });
});
