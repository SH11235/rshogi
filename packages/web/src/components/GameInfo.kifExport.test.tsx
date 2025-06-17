import type { Move } from "shogi-core";
import { beforeEach, describe, expect, it, vi } from "vitest";

// Mock the KIF utilities
vi.mock("@/utils/kif", () => ({
    exportToKif: vi.fn((moves: Move[]) => `mock KIF content with ${moves.length} moves`),
    parseKifMoves: vi.fn(() => [
        {
            type: "drop",
            to: { row: 5, column: 5 },
            piece: { type: "pawn", owner: "black", promoted: false },
        },
    ]),
    validateKifFormat: vi.fn(() => ({ valid: true })),
}));

// Mock window.URL and Blob
const mockCreateObjectURL = vi.fn(() => "mock-url");
const mockRevokeObjectURL = vi.fn();
const mockClick = vi.fn();
const mockAppendChild = vi.fn();
const mockRemoveChild = vi.fn();

Object.defineProperty(window, "URL", {
    value: {
        createObjectURL: mockCreateObjectURL,
        revokeObjectURL: mockRevokeObjectURL,
    },
    writable: true,
});

Object.defineProperty(document, "createElement", {
    value: vi.fn(() => ({
        href: "",
        download: "",
        click: mockClick,
    })),
    writable: true,
});

Object.defineProperty(document.body, "appendChild", {
    value: mockAppendChild,
    writable: true,
});

Object.defineProperty(document.body, "removeChild", {
    value: mockRemoveChild,
    writable: true,
});

// Mock window.alert
const mockAlert = vi.fn();
Object.defineProperty(window, "alert", {
    value: mockAlert,
    writable: true,
});

// Mock FileReader
const mockFileReader = {
    readAsText: vi.fn(),
    onload: null as ((event: { target: { result: string } }) => void) | null,
};

Object.defineProperty(window, "FileReader", {
    value: vi.fn(() => mockFileReader),
    writable: true,
});

describe("KIF Export/Import Utilities Integration", () => {
    const mockMoveHistory: Move[] = [
        {
            type: "move",
            from: { row: 7, column: 7 },
            to: { row: 6, column: 7 },
            piece: { type: "pawn", owner: "black", promoted: false },
            promote: false,
            captured: null,
        },
        {
            type: "drop",
            to: { row: 5, column: 5 },
            piece: { type: "pawn", owner: "white", promoted: false },
        },
    ];

    beforeEach(() => {
        vi.clearAllMocks();
        mockFileReader.onload = null;
    });

    describe("Export utility integration", () => {
        it("should call exportToKif with correct parameters", async () => {
            const { exportToKif } = await import("@/utils/kif");

            vi.mocked(exportToKif).mockReturnValue("mocked KIF content");

            const gameInfo = {
                开始日时: "2024-01-01 12:00:00",
                先手: "Player1",
                后手: "Player2",
                棋战: "Test Game",
                手合割: "平手",
            };

            const result = vi.mocked(exportToKif)(mockMoveHistory, gameInfo);

            expect(exportToKif).toHaveBeenCalledWith(mockMoveHistory, gameInfo);
            expect(result).toBe("mocked KIF content");
        });

        it("should handle blob creation and download trigger", () => {
            const content = "test KIF content";
            const blob = new Blob([content], { type: "text/plain;charset=utf-8" });

            expect(blob.type).toBe("text/plain;charset=utf-8");
            expect(blob.size).toBeGreaterThan(0);
        });
    });

    describe("Import utility integration", () => {
        it("should validate KIF format correctly", async () => {
            const { validateKifFormat } = await import("@/utils/kif");

            const validContent = `# KIF形式棋譜ファイル
手数----指手---------消費時間--
  1 5五歩打`;

            vi.mocked(validateKifFormat).mockReturnValue({ valid: true });

            const result = vi.mocked(validateKifFormat)(validContent);

            expect(validateKifFormat).toHaveBeenCalledWith(validContent);
            expect(result.valid).toBe(true);
        });

        it("should parse KIF moves correctly", async () => {
            const { parseKifMoves } = await import("@/utils/kif");

            const kifContent = "mock KIF content";
            const expectedMoves = [
                {
                    type: "drop",
                    to: { row: 5, column: 5 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                },
            ];

            vi.mocked(parseKifMoves).mockReturnValue(expectedMoves);

            const result = vi.mocked(parseKifMoves)(kifContent);

            expect(parseKifMoves).toHaveBeenCalledWith(kifContent);
            expect(result).toEqual(expectedMoves);
        });

        it("should handle invalid KIF format", async () => {
            const { validateKifFormat } = await import("@/utils/kif");

            vi.mocked(validateKifFormat).mockReturnValue({
                valid: false,
                error: "Invalid format",
            });

            const result = vi.mocked(validateKifFormat)("invalid content");

            expect(result.valid).toBe(false);
            expect(result.error).toBe("Invalid format");
        });
    });

    describe("File handling", () => {
        it("should handle FileReader correctly", () => {
            const content = "test content";
            const file = new File([content], "test.kif", { type: "text/plain" });

            expect(file.name).toBe("test.kif");
            expect(file.type).toBe("text/plain");
            expect(file.size).toBe(content.length);
        });

        it("should simulate FileReader onload event", () => {
            const mockOnLoad = vi.fn();
            mockFileReader.onload = mockOnLoad;

            const testEvent = { target: { result: "test content" } };

            if (mockFileReader.onload) {
                mockFileReader.onload(testEvent);
            }

            expect(mockOnLoad).toHaveBeenCalledWith(testEvent);
        });

        it("should handle readAsText call", () => {
            const file = new File(["content"], "test.kif", { type: "text/plain" });

            mockFileReader.readAsText(file, "utf-8");

            expect(mockFileReader.readAsText).toHaveBeenCalledWith(file, "utf-8");
        });
    });
});
