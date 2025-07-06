import type { OpeningBookReaderWasm } from "@/wasm/shogi_core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { OpeningBookService } from "../openingBook";

// WASMモジュールのモック
vi.mock("@/wasm/shogi_core", () => ({
    OpeningBookReaderWasm: vi.fn().mockImplementation(() => {
        let loaded = false;
        let positionCount = 0;

        return {
            load_data: vi.fn().mockImplementation((data: Uint8Array) => {
                // データサイズに基づいてポジション数をシミュレート
                positionCount = Math.floor(data.length / 100);
                loaded = true;
                return `Loaded ${positionCount} positions`;
            }),
            find_moves: vi.fn().mockReturnValue("[]"),
            get position_count() {
                return positionCount;
            },
            get is_loaded() {
                return loaded;
            },
        };
    }),
}));

describe("OpeningBookService", () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    it("should create instance", () => {
        // Arrange & Act
        const service = new OpeningBookService();

        // Assert
        expect(service).toBeDefined();
        expect(service.isInitialized()).toBe(false);
    });

    describe("WASM Initialization", () => {
        it("should initialize WASM module on first call", async () => {
            // Arrange
            const service = new OpeningBookService();
            expect(service.isInitialized()).toBe(false);

            // Act
            await service.initialize();

            // Assert
            expect(service.isInitialized()).toBe(true);
            const reader = service.getReader();
            expect(reader).toBeDefined();
            expect(reader).toHaveProperty("load_data");
            expect(reader).toHaveProperty("find_moves");
        });

        it("should not reinitialize if already initialized", async () => {
            // Arrange
            const service = new OpeningBookService();
            await service.initialize();
            const firstReader = service.getReader();

            // Act
            await service.initialize();

            // Assert
            expect(service.getReader()).toBe(firstReader);
        });

        it("should handle initialization errors gracefully", async () => {
            // Arrange
            const service = new OpeningBookService();
            const mockError = new Error("WASM initialization failed");

            // WASMモジュールのコンストラクタをエラーで置き換え
            const { OpeningBookReaderWasm } = await import("@/wasm/shogi_core");
            vi.mocked(OpeningBookReaderWasm).mockImplementationOnce(() => {
                throw mockError;
            });

            // Act & Assert
            await expect(service.initialize()).rejects.toThrow("WASM initialization failed");
            expect(service.isInitialized()).toBe(false);
        });
    });

    describe("Data Loading", () => {
        it("should load early level book data", async () => {
            // Arrange
            const service = new OpeningBookService();
            const onProgress = vi.fn();

            // fetchをモック（実際のgzipデータをシミュレート）
            global.fetch = vi.fn().mockResolvedValue({
                headers: new Headers({ "content-length": "300" }),
                body: {
                    getReader: () => ({
                        read: vi
                            .fn()
                            .mockResolvedValueOnce({ value: new Uint8Array(150), done: false })
                            .mockResolvedValueOnce({ value: new Uint8Array(150), done: false })
                            .mockResolvedValueOnce({ done: true }),
                    }),
                },
            });

            // Act
            await service.loadBook("early", onProgress);

            // Assert
            expect(fetch).toHaveBeenCalledWith("/data/opening_book_early.bin.gz");
            expect(onProgress).toHaveBeenCalledWith({
                phase: "downloading",
                loaded: 150,
                total: 300,
            });
            expect(onProgress).toHaveBeenCalledWith({
                phase: "downloading",
                loaded: 300,
                total: 300,
            });
            expect(onProgress).toHaveBeenCalledWith({
                phase: "decompressing",
                loaded: 0,
                total: 100,
            });
            expect(onProgress).toHaveBeenCalledWith({
                phase: "decompressing",
                loaded: 100,
                total: 100,
            });

            const reader = service.getReader();
            expect(reader?.is_loaded).toBe(true);
            expect(reader?.position_count).toBeGreaterThan(0);
        });

        it("should not reload same level book", async () => {
            // Arrange
            const service = new OpeningBookService();

            // 最初のロード
            global.fetch = vi.fn().mockResolvedValue({
                headers: new Headers({ "content-length": "100" }),
                body: {
                    getReader: () => ({
                        read: vi.fn().mockResolvedValueOnce({ done: true }),
                    }),
                },
            });

            await service.loadBook("early");
            vi.clearAllMocks();

            // Act: 同じレベルを再度ロード
            await service.loadBook("early");

            // Assert
            expect(fetch).not.toHaveBeenCalled();
        });

        it("should handle network errors gracefully", async () => {
            // Arrange
            const service = new OpeningBookService();
            const onProgress = vi.fn();

            // fetchをエラーでモック
            global.fetch = vi.fn().mockRejectedValue(new Error("Network error"));

            // Act & Assert
            await expect(service.loadBook("full", onProgress)).rejects.toThrow("Network error");
        });

        it("should load different book levels", async () => {
            // Arrange
            const service = new OpeningBookService();

            // fetchをモック
            global.fetch = vi.fn().mockResolvedValue({
                headers: new Headers({ "content-length": "100" }),
                body: {
                    getReader: () => ({
                        read: vi.fn().mockResolvedValueOnce({ done: true }),
                    }),
                },
            });

            // Act
            await service.loadBook("standard");

            // Assert
            expect(fetch).toHaveBeenCalledWith("/data/opening_book_standard.bin.gz");
        });
    });

    it("should download book data", async () => {
        // Arrange
        const service = new OpeningBookService();
        await service.initialize();

        let progressCalled = false;
        const onProgress = () => {
            progressCalled = true;
        };

        // fetchをモック
        global.fetch = vi.fn().mockResolvedValue({
            headers: new Headers({ "content-length": "100" }),
            body: {
                getReader: () => ({
                    read: vi
                        .fn()
                        .mockResolvedValueOnce({ value: new Uint8Array(50), done: false })
                        .mockResolvedValueOnce({ done: true }),
                }),
            },
        });

        // Act
        await service.loadBook("early", onProgress);

        // Assert
        expect(fetch).toHaveBeenCalledWith("/data/opening_book_early.bin.gz");
        expect(progressCalled).toBe(true);
    });

    it("should handle load errors", async () => {
        // Arrange
        const service = new OpeningBookService();
        await service.initialize();

        // fetchをエラーでモック
        global.fetch = vi.fn().mockRejectedValue(new Error("Network error"));

        // Act & Assert
        await expect(service.loadBook("early")).rejects.toThrow("Network error");
    });

    describe("Move Finding", () => {
        it("should find moves for initial position", async () => {
            // Arrange
            const service = new OpeningBookService();
            const initialSfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

            // WASMモジュールのfind_movesをモック
            const mockMoves = [
                { notation: "7g7f", evaluation: 50, depth: 20 },
                { notation: "2g2f", evaluation: 45, depth: 18 },
            ];

            await service.initialize();
            const reader = service.getReader();
            if (reader) {
                vi.mocked(reader.find_moves).mockReturnValue(JSON.stringify(mockMoves));
            }

            // データを先に読み込む
            global.fetch = vi.fn().mockResolvedValue({
                headers: new Headers({ "content-length": "300" }),
                body: {
                    getReader: () => ({
                        read: vi.fn().mockResolvedValueOnce({ done: true }),
                    }),
                },
            });
            await service.loadBook("early");

            // Act
            const moves = await service.findMoves(initialSfen);

            // Assert
            expect(moves).toEqual(mockMoves);
            expect(reader?.find_moves).toHaveBeenCalledWith(initialSfen);
        });

        it("should return empty array for unknown position", async () => {
            // Arrange
            const service = new OpeningBookService();
            await service.initialize();

            const reader = service.getReader();
            if (reader) {
                vi.mocked(reader.find_moves).mockReturnValue("[]");
            }

            // Act
            const moves = await service.findMoves("unknown-sfen");

            // Assert
            expect(moves).toEqual([]);
        });

        it("should handle JSON parse errors gracefully", async () => {
            // Arrange
            const service = new OpeningBookService();
            await service.initialize();

            const reader = service.getReader();
            if (reader) {
                vi.mocked(reader.find_moves).mockReturnValue("invalid json");
            }

            // Act
            const moves = await service.findMoves("test-sfen");

            // Assert
            expect(moves).toEqual([]);
        });

        it("should return empty array if not initialized", async () => {
            // Arrange
            const service = new OpeningBookService();

            // Act
            const moves = await service.findMoves("test-sfen");

            // Assert
            expect(moves).toEqual([]);
        });
    });
});
