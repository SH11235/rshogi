import { beforeEach, describe, expect, it } from "vitest";
import {
    formatSquare,
    getLocale,
    setLocale,
    translateErrorMessage,
    translateGameTerm,
    translatePieceName,
} from "./i18n";

describe("国際化ユーティリティ", () => {
    beforeEach(() => {
        // テスト前に日本語にリセット
        setLocale("ja");
    });

    describe("言語設定", () => {
        it("デフォルトは日本語", () => {
            expect(getLocale()).toBe("ja");
        });

        it("言語を変更できる", () => {
            setLocale("en");
            expect(getLocale()).toBe("en");
        });
    });

    describe("駒名の翻訳", () => {
        it("日本語で駒名を取得できる", () => {
            expect(translatePieceName("pawn")).toBe("歩");
            expect(translatePieceName("king")).toBe("王");
            expect(translatePieceName("gyoku")).toBe("玉");
        });

        it("英語で駒名を取得できる", () => {
            expect(translatePieceName("pawn", false, "en")).toBe("Pawn");
            expect(translatePieceName("king", false, "en")).toBe("King");
            expect(translatePieceName("gyoku", false, "en")).toBe("Jeweled General");
        });

        it("成駒の名前を取得できる", () => {
            expect(translatePieceName("pawn", true, "ja")).toBe("と");
            expect(translatePieceName("bishop", true, "ja")).toBe("馬");
            expect(translatePieceName("rook", true, "ja")).toBe("龍");

            expect(translatePieceName("pawn", true, "en")).toBe("Tokin");
            expect(translatePieceName("bishop", true, "en")).toBe("Horse");
            expect(translatePieceName("rook", true, "en")).toBe("Dragon");
        });

        it("グローバル設定を使用する", () => {
            setLocale("en");
            expect(translatePieceName("pawn")).toBe("Pawn");

            setLocale("ja");
            expect(translatePieceName("pawn")).toBe("歩");
        });
    });

    describe("ゲーム用語の翻訳", () => {
        it("日本語のゲーム用語を取得できる", () => {
            expect(translateGameTerm("black")).toBe("先手");
            expect(translateGameTerm("white")).toBe("後手");
            expect(translateGameTerm("checkmate")).toBe("詰み");
            expect(translateGameTerm("nifu")).toBe("二歩");
        });

        it("英語のゲーム用語を取得できる", () => {
            expect(translateGameTerm("black", "en")).toBe("Black");
            expect(translateGameTerm("white", "en")).toBe("White");
            expect(translateGameTerm("checkmate", "en")).toBe("Checkmate");
            expect(translateGameTerm("nifu", "en")).toBe("Double Pawn");
        });

        it("グローバル設定を使用する", () => {
            setLocale("en");
            expect(translateGameTerm("turn")).toBe("Turn");

            setLocale("ja");
            expect(translateGameTerm("turn")).toBe("手番");
        });
    });

    describe("エラーメッセージの翻訳", () => {
        it("日本語のエラーメッセージを取得できる", () => {
            expect(translateErrorMessage("nifu")).toBe("二歩です");
            expect(translateErrorMessage("uchifuzume")).toBe("打ち歩詰めです");
            expect(translateErrorMessage("invalidMove")).toBe("無効な手です");
        });

        it("英語のエラーメッセージを取得できる", () => {
            expect(translateErrorMessage("nifu", "en")).toBe("Double pawn");
            expect(translateErrorMessage("uchifuzume", "en")).toBe("Pawn drop mate");
            expect(translateErrorMessage("invalidMove", "en")).toBe("Invalid move");
        });

        it("グローバル設定を使用する", () => {
            setLocale("en");
            expect(translateErrorMessage("deadPiece")).toBe("Immobile piece");

            setLocale("ja");
            expect(translateErrorMessage("deadPiece")).toBe("行き所のない駒です");
        });
    });

    describe("座標の表示形式", () => {
        it("日本語形式で座標を表示できる", () => {
            expect(formatSquare(1, 1, "ja")).toBe("1一");
            expect(formatSquare(5, 5, "ja")).toBe("5五");
            expect(formatSquare(9, 9, "ja")).toBe("9九");
        });

        it("英語形式で座標を表示できる", () => {
            expect(formatSquare(1, 1, "en")).toBe("1a");
            expect(formatSquare(5, 5, "en")).toBe("5e");
            expect(formatSquare(9, 9, "en")).toBe("9i");
        });

        it("グローバル設定を使用する", () => {
            setLocale("ja");
            expect(formatSquare(7, 6)).toBe("6七");

            setLocale("en");
            expect(formatSquare(7, 6)).toBe("6g");
        });
    });
});
