import { describe, expect, it } from "vitest";
import {
    type LegacyPiece,
    type ModernPiece,
    canPromote,
    convertLegacyPieceType,
    convertToLegacyPieceKind,
    createLegacyPiece,
    createModernPiece,
    getPieceName,
    isLegacyPiece,
    isModernPiece,
    isPawn,
    isRoyalPiece,
    promote,
    toLegacyPiece,
    toModernPiece,
} from "./piece";

describe("駒の型とユーティリティ", () => {
    describe("型ガード", () => {
        it("モダン形式の駒を正しく識別する", () => {
            const modernPiece = createModernPiece("pawn", "black");
            expect(isModernPiece(modernPiece)).toBe(true);
            expect(isLegacyPiece(modernPiece)).toBe(false);
        });

        it("レガシー形式の駒を正しく識別する", () => {
            const legacyPiece = createLegacyPiece("歩", "black");
            expect(isLegacyPiece(legacyPiece)).toBe(true);
            expect(isModernPiece(legacyPiece)).toBe(false);
        });
    });

    describe("駒のタイプチェック", () => {
        it("歩を正しく識別する", () => {
            const modernPawn = createModernPiece("pawn", "black");
            const legacyPawn = createLegacyPiece("歩", "black");

            expect(isPawn(modernPawn)).toBe(true);
            expect(isPawn(legacyPawn)).toBe(true);
        });

        it("王や玉を正しく識別する", () => {
            const king = createModernPiece("king", "black");
            const gyoku = createModernPiece("gyoku", "white");
            const legacyKing = createLegacyPiece("王", "black");
            const legacyGyoku = createLegacyPiece("玉", "white");

            expect(isRoyalPiece(king)).toBe(true);
            expect(isRoyalPiece(gyoku)).toBe(true);
            expect(isRoyalPiece(legacyKing)).toBe(true);
            expect(isRoyalPiece(legacyGyoku)).toBe(true);
        });
    });

    describe("成り判定", () => {
        it("成れる駒を正しく識別する", () => {
            const pawn = createModernPiece("pawn", "black");
            const gold = createModernPiece("gold", "black");

            expect(canPromote(pawn)).toBe(true);
            expect(canPromote(gold)).toBe(false);
        });

        it("既に成っている駒は成れない", () => {
            const promotedPawn = createModernPiece("pawn", "black", true);
            expect(canPromote(promotedPawn)).toBe(false);
        });

        it("駒を成らせることができる", () => {
            const pawn = createModernPiece("pawn", "black");
            const promotedPawn = promote(pawn);

            expect(promotedPawn.promoted).toBe(true);
            expect(promotedPawn.type).toBe("pawn");
        });
    });

    describe("国際化対応", () => {
        it("日本語の駒名を取得できる", () => {
            const pawn = createModernPiece("pawn", "black");
            const promotedPawn = createModernPiece("pawn", "black", true);

            expect(getPieceName(pawn, "ja")).toBe("歩");
            expect(getPieceName(promotedPawn, "ja")).toBe("と");
        });

        it("英語の駒名を取得できる", () => {
            const pawn = createModernPiece("pawn", "black");
            const promotedPawn = createModernPiece("pawn", "black", true);

            expect(getPieceName(pawn, "en")).toBe("Pawn");
            expect(getPieceName(promotedPawn, "en")).toBe("Tokin");
        });

        it("レガシー形式でも駒名を取得できる", () => {
            const legacyPawn = createLegacyPiece("歩", "black");
            const legacyPromotedPawn = createLegacyPiece("歩", "black", true);

            expect(getPieceName(legacyPawn, "ja")).toBe("歩");
            expect(getPieceName(legacyPromotedPawn, "ja")).toBe("と");
            expect(getPieceName(legacyPawn, "en")).toBe("Pawn");
            expect(getPieceName(legacyPromotedPawn, "en")).toBe("Tokin");
        });
    });

    describe("形式変換", () => {
        it("レガシー形式からモダン形式に変換できる", () => {
            const legacyPiece = createLegacyPiece("歩", "black", true);
            const modernPiece = toModernPiece(legacyPiece);

            expect(isModernPiece(modernPiece)).toBe(true);
            expect(modernPiece.type).toBe("pawn");
            expect(modernPiece.promoted).toBe(true);
            expect(modernPiece.owner).toBe("black");
        });

        it("モダン形式からレガシー形式に変換できる", () => {
            const modernPiece = createModernPiece("bishop", "white", false);
            const legacyPiece = toLegacyPiece(modernPiece);

            expect(isLegacyPiece(legacyPiece)).toBe(true);
            expect(legacyPiece.kind).toBe("角");
            expect(legacyPiece.promoted).toBe(false);
            expect(legacyPiece.owner).toBe("white");
        });

        it("駒タイプの相互変換ができる", () => {
            expect(convertLegacyPieceType("歩")).toBe("pawn");
            expect(convertLegacyPieceType("王")).toBe("king");
            expect(convertLegacyPieceType("玉")).toBe("gyoku");

            expect(convertToLegacyPieceKind("pawn")).toBe("歩");
            expect(convertToLegacyPieceKind("king")).toBe("王");
            expect(convertToLegacyPieceKind("gyoku")).toBe("玉");
        });
    });

    describe("後方互換性", () => {
        it("既存のコードがレガシー形式で動作する", () => {
            const legacyPiece: LegacyPiece = {
                kind: "歩",
                promoted: false,
                owner: "black",
            };

            expect(canPromote(legacyPiece)).toBe(true);

            const promoted = promote(legacyPiece);
            expect(promoted.promoted).toBe(true);
        });

        it("新しいコードがモダン形式で動作する", () => {
            const modernPiece: ModernPiece = {
                type: "rook",
                promoted: false,
                owner: "white",
            };

            expect(canPromote(modernPiece)).toBe(true);

            const promoted = promote(modernPiece);
            expect(promoted.promoted).toBe(true);
        });
    });
});
