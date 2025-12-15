import { describe, expect, it } from "vitest";
import { parseUsiInput } from "./kifuUtils";

describe("kifuUtils", () => {
    describe("parseUsiInput", () => {
        it("'startpos moves' 形式の入力を解析する", () => {
            const result = parseUsiInput("startpos moves 7g7f 3c3d");
            expect(result).toEqual(["7g7f", "3c3d"]);
        });

        it("'moves' キーワードなしの入力を解析する", () => {
            const result = parseUsiInput("7g7f 3c3d 2g2f");
            expect(result).toEqual(["7g7f", "3c3d", "2g2f"]);
        });

        it("SFEN + moves 形式の入力を解析する", () => {
            const sfen =
                "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1 moves 7g7f";
            const result = parseUsiInput(sfen);
            expect(result).toEqual(["7g7f"]);
        });

        it("空文字列は空配列を返す", () => {
            const result = parseUsiInput("");
            expect(result).toEqual([]);
        });

        it("空白のみの入力は空配列を返す", () => {
            const result = parseUsiInput("   ");
            expect(result).toEqual([]);
        });

        it("'moves' の後に何もない場合は空配列を返す", () => {
            const result = parseUsiInput("startpos moves");
            expect(result).toEqual([]);
        });

        it("'moves' の後に空白のみの場合は空配列を返す", () => {
            const result = parseUsiInput("startpos moves   ");
            expect(result).toEqual([]);
        });

        it("複数の空白文字で区切られた入力を正しく解析する", () => {
            const result = parseUsiInput("7g7f    3c3d  2g2f");
            expect(result).toEqual(["7g7f", "3c3d", "2g2f"]);
        });

        it("改行やタブを含む入力を正しく解析する", () => {
            const result = parseUsiInput("7g7f\t3c3d\n2g2f");
            expect(result).toEqual(["7g7f", "3c3d", "2g2f"]);
        });

        it("前後の空白を除去する", () => {
            const result = parseUsiInput("  startpos moves 7g7f 3c3d  ");
            expect(result).toEqual(["7g7f", "3c3d"]);
        });
    });
});
