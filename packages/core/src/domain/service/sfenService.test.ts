import { describe, expect, it } from "vitest";
import { modernInitialBoard } from "../initialBoard";
import { getPiece } from "../model/board";
import { initialHands } from "./moveService";
import {
    INITIAL_SFEN,
    exportToSfen,
    isInitialPosition,
    parseSfen,
    validateSfenFormat,
} from "./sfenService";

describe("sfenService", () => {
    describe("exportToSfen", () => {
        it("exports initial position correctly", () => {
            const sfen = exportToSfen(modernInitialBoard, initialHands(), "black", 1);
            expect(sfen).toBe(`sfen ${INITIAL_SFEN}`);
        });

        it("exports position with captured pieces", () => {
            const hands = initialHands();
            hands.black.歩 = 2;
            hands.white.銀 = 1;
            hands.white.桂 = 1;

            const sfen = exportToSfen(modernInitialBoard, hands, "white", 10);
            expect(sfen).toContain(" w ");
            expect(sfen).toContain("sn2P");
            expect(sfen).toContain(" 10");
        });

        it("exports position with promoted pieces", () => {
            const board = { ...modernInitialBoard };
            // 成銀を5五に配置
            board["55"] = { type: "silver", owner: "black", promoted: true };

            const sfen = exportToSfen(board, initialHands(), "black", 1);
            expect(sfen).toContain("+S");
        });
    });

    describe("parseSfen", () => {
        it("parses initial position correctly", () => {
            const position = parseSfen(INITIAL_SFEN);

            expect(position.currentPlayer).toBe("black");
            expect(position.moveNumber).toBe(1);

            // 初期配置の確認（一部）
            const piece11 = getPiece(position.board, { row: 1, column: 1 });
            expect(piece11).toEqual({ type: "lance", owner: "white", promoted: false });

            const piece99 = getPiece(position.board, { row: 9, column: 9 });
            expect(piece99).toEqual({ type: "lance", owner: "black", promoted: false });
        });

        it("parses position with captured pieces", () => {
            const sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w 2Psn 10";
            const position = parseSfen(sfen);

            expect(position.currentPlayer).toBe("white");
            expect(position.moveNumber).toBe(10);
            expect(position.hands.black.歩).toBe(2);
            expect(position.hands.white.銀).toBe(1);
            expect(position.hands.white.桂).toBe(1);
        });

        it("parses position with promoted pieces", () => {
            const sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/4+S4/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
            const position = parseSfen(sfen);

            const piece55 = getPiece(position.board, { row: 5, column: 5 });
            expect(piece55).toEqual({ type: "silver", owner: "black", promoted: true });
        });

        it("parses position with multiple captured pieces", () => {
            const sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 10P5p2S 1";
            const position = parseSfen(sfen);

            expect(position.hands.black.歩).toBe(10);
            expect(position.hands.white.歩).toBe(5);
            expect(position.hands.black.銀).toBe(2);
        });

        it("handles sfen prefix", () => {
            const position1 = parseSfen(`sfen ${INITIAL_SFEN}`);
            const position2 = parseSfen(INITIAL_SFEN);

            expect(position1.currentPlayer).toBe(position2.currentPlayer);
            expect(position1.moveNumber).toBe(position2.moveNumber);
        });
    });

    describe("validateSfenFormat", () => {
        it("validates correct SFEN", () => {
            const result = validateSfenFormat(INITIAL_SFEN);
            expect(result.valid).toBe(true);
            expect(result.error).toBeUndefined();
        });

        it("validates SFEN with prefix", () => {
            const result = validateSfenFormat(`sfen ${INITIAL_SFEN}`);
            expect(result.valid).toBe(true);
        });

        it("rejects invalid board format", () => {
            const result = validateSfenFormat("invalid/board/format b - 1");
            expect(result.valid).toBe(false);
            expect(result.error).toContain("無効なSFEN");
        });

        it("rejects missing components", () => {
            const result = validateSfenFormat(
                "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL",
            );
            expect(result.valid).toBe(false);
            expect(result.error).toContain("最低4つの要素が必要");
        });

        it("rejects invalid piece characters", () => {
            const result = validateSfenFormat(
                "Xnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            );
            expect(result.valid).toBe(false);
            expect(result.error).toContain("無効なSFEN駒");
        });

        it("rejects invalid turn", () => {
            const result = validateSfenFormat(
                "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL x - 1",
            );
            expect(result.valid).toBe(false);
        });

        it("rejects invalid move number", () => {
            const result = validateSfenFormat(
                "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - abc",
            );
            expect(result.valid).toBe(false);
            expect(result.error).toContain("無効な手数");
        });
    });

    describe("isInitialPosition", () => {
        it("recognizes initial position", () => {
            const result = isInitialPosition(modernInitialBoard, initialHands());
            expect(result).toBe(true);
        });

        it("rejects position with moved pieces", () => {
            const board = { ...modernInitialBoard };
            // 歩を一つ前に
            const piece = board["77"];
            if (piece) {
                board["76"] = piece;
                board["77"] = null;
            }

            const result = isInitialPosition(board, initialHands());
            expect(result).toBe(false);
        });

        it("rejects position with captured pieces", () => {
            const hands = initialHands();
            hands.black.歩 = 1;

            const result = isInitialPosition(modernInitialBoard, hands);
            expect(result).toBe(false);
        });
    });

    describe("round-trip conversion", () => {
        it("preserves initial position", () => {
            const originalSfen = INITIAL_SFEN;
            const position = parseSfen(originalSfen);
            const exportedSfen = exportToSfen(
                position.board,
                position.hands,
                position.currentPlayer,
                position.moveNumber,
            );

            expect(exportedSfen).toBe(`sfen ${originalSfen}`);
        });

        it("preserves complex position", () => {
            const originalSfen =
                "ln1gk2nl/1r2sgs2/p1pppp1pp/1p4p2/9/2P4P1/PP1PPPP1P/1SG3GR1/LN2K2NL w Bb2P 25";
            const position = parseSfen(originalSfen);
            const exportedSfen = exportToSfen(
                position.board,
                position.hands,
                position.currentPlayer,
                position.moveNumber,
            );

            expect(exportedSfen).toBe(`sfen ${originalSfen}`);
        });
    });
});
