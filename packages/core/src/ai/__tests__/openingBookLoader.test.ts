import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { OpeningBookLoader } from "../openingBookLoader";
import { OpeningBook } from "../openingBook";

// Fetch APIのモック
global.fetch = vi.fn();

describe("OpeningBookLoader", () => {
    let loader: OpeningBookLoader;

    beforeEach(() => {
        loader = new OpeningBookLoader();
        vi.clearAllMocks();
    });

    afterEach(() => {
        vi.restoreAllMocks();
    });

    describe("load", () => {
        it("should load and parse binary opening book file", async () => {
            // バイナリデータのモック（簡易版）
            const mockBinaryData = new ArrayBuffer(100);
            const mockResponse = {
                ok: true,
                arrayBuffer: vi.fn().mockResolvedValue(mockBinaryData),
            };
            (global.fetch as any).mockResolvedValue(mockResponse);

            const book = await loader.load("/data/opening_book_tournament.bin.gz");

            expect(book).toBeInstanceOf(OpeningBook);
            expect(fetch).toHaveBeenCalledWith("/data/opening_book_tournament.bin.gz");
        });

        it("should throw error when file not found", async () => {
            const mockResponse = {
                ok: false,
                status: 404,
            };
            (global.fetch as any).mockResolvedValue(mockResponse);

            await expect(loader.load("/data/nonexistent.bin.gz")).rejects.toThrow(
                "Failed to load opening book",
            );
        });

        it("should handle network errors", async () => {
            (global.fetch as any).mockRejectedValue(new Error("Network error"));

            await expect(loader.load("/data/opening_book.bin.gz")).rejects.toThrow(
                "Failed to load opening book",
            );
        });
    });

    describe("loadFromFallback", () => {
        it("should create opening book from generated data", () => {
            const book = loader.loadFromFallback();

            expect(book).toBeInstanceOf(OpeningBook);
            expect(book.size()).toBeGreaterThan(0);
        });

        it("should include basic openings", () => {
            const book = loader.loadFromFallback();

            // 初期局面の定跡が含まれているか確認
            const initialPosition = {
                board: { _testInitialPosition: true } as any,
                hands: {
                    black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
                    white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
                },
                currentPlayer: "black" as const,
            };

            const moves = book.findMoves(initialPosition);
            expect(moves.length).toBeGreaterThan(0);

            // 基本的な初手が含まれているか
            const notations = moves.map((m) => m.notation);
            expect(notations).toContain("7g7f");
            expect(notations).toContain("2g2f");
        });
    });

    describe("parseBookData", () => {
        it("should parse binary data correctly", () => {
            // 簡易的なバイナリフォーマットのモック
            // 実際のフォーマットに合わせて調整が必要
            const encoder = new TextEncoder();
            const positionBytes = encoder.encode(
                "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -",
            );
            const notationBytes = encoder.encode("7g7f");

            // バイナリ構造：[エントリ数(4bytes)][位置長(2bytes)][位置][手数(2bytes)][深さ(1byte)][手情報...]
            const buffer = new ArrayBuffer(1000);
            const view = new DataView(buffer);
            let offset = 0;

            // エントリ数
            view.setUint32(offset, 1, true);
            offset += 4;

            // 最初のエントリ
            // 位置長
            view.setUint16(offset, positionBytes.length, true);
            offset += 2;

            // 位置データ
            const uint8View = new Uint8Array(buffer);
            uint8View.set(positionBytes, offset);
            offset += positionBytes.length;

            // 手数
            view.setUint16(offset, 1, true);
            offset += 2;

            // 深さ
            view.setUint8(offset, 20);
            offset += 1;

            // 手情報
            // 記譜長
            view.setUint8(offset, notationBytes.length);
            offset += 1;

            // 記譜
            uint8View.set(notationBytes, offset);
            offset += notationBytes.length;

            // 重み
            view.setUint16(offset, 100, true);
            offset += 2;

            // フラグ（名前や注釈なし）
            view.setUint8(offset, 0);

            const book = (loader as any).parseBookData(buffer);
            expect(book).toBeInstanceOf(OpeningBook);
            expect(book.size()).toBe(1);
        });
    });

    describe("loadForDifficulty", () => {
        it("should load correct file for beginner difficulty", async () => {
            const mockResponse = {
                ok: true,
                arrayBuffer: vi.fn().mockResolvedValue(new ArrayBuffer(100)),
            };
            (global.fetch as any).mockResolvedValue(mockResponse);

            await loader.loadForDifficulty("beginner");

            expect(fetch).toHaveBeenCalledWith("/data/opening_book_tournament.bin.gz");
        });

        it("should load correct file for intermediate difficulty", async () => {
            const mockResponse = {
                ok: true,
                arrayBuffer: vi.fn().mockResolvedValue(new ArrayBuffer(100)),
            };
            (global.fetch as any).mockResolvedValue(mockResponse);

            await loader.loadForDifficulty("intermediate");

            expect(fetch).toHaveBeenCalledWith("/data/opening_book_standard.bin.gz");
        });

        it("should load correct file for advanced difficulty", async () => {
            const mockResponse = {
                ok: true,
                arrayBuffer: vi.fn().mockResolvedValue(new ArrayBuffer(100)),
            };
            (global.fetch as any).mockResolvedValue(mockResponse);

            await loader.loadForDifficulty("advanced");

            expect(fetch).toHaveBeenCalledWith("/data/opening_book_yokofudori.bin.gz");
        });

        it("should use fallback when loading fails", async () => {
            (global.fetch as any).mockRejectedValue(new Error("Network error"));

            const book = await loader.loadForDifficulty("beginner");

            expect(book).toBeInstanceOf(OpeningBook);
            expect(book.size()).toBeGreaterThan(0);
        });
    });

    describe("decompressGzip", () => {
        it("should decompress gzipped data", async () => {
            // 実際のgzipデータのモック
            // ブラウザ環境でのDecompressionStreamのテスト
            const mockCompressedData = new ArrayBuffer(50);
            const mockDecompressedData = new ArrayBuffer(100);

            // DecompressionStreamのモック
            const mockStream = {
                readable: {
                    getReader: vi.fn().mockReturnValue({
                        read: vi
                            .fn()
                            .mockResolvedValueOnce({
                                done: false,
                                value: new Uint8Array(mockDecompressedData),
                            })
                            .mockResolvedValueOnce({ done: true }),
                    }),
                },
                writable: {
                    getWriter: vi.fn().mockReturnValue({
                        write: vi.fn().mockResolvedValue(undefined),
                        close: vi.fn().mockResolvedValue(undefined),
                    }),
                },
            };

            global.DecompressionStream = vi.fn().mockImplementation(() => mockStream);

            const result = await (loader as any).decompressGzip(mockCompressedData);

            expect(result).toBeInstanceOf(ArrayBuffer);
            expect(result.byteLength).toBe(100);
        });
    });
});
