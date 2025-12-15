import { describe, expect, it } from "vitest";
import { parseUsiInput } from "./kifuUtils";

describe("parseUsiInput", () => {
    it("moves 以降を抽出する", () => {
        const input = "position startpos moves 7g7f 3c3d";
        expect(parseUsiInput(input)).toEqual(["7g7f", "3c3d"]);
    });

    it("moves が複数回出ても最初の moves 以降を解釈する", () => {
        const input = "position startpos moves 7g7f moves 3c3d";
        expect(parseUsiInput(input)).toEqual(["7g7f", "moves", "3c3d"]);
    });

    it("startpos/sfen のみの場合は空配列を返す", () => {
        expect(parseUsiInput("position startpos")).toEqual([]);
        expect(parseUsiInput("startpos")).toEqual([]);
        expect(parseUsiInput("position sfen some_sfen_string")).toEqual([]);
        expect(parseUsiInput("sfen some_sfen_string")).toEqual([]);
    });

    it("moves が無い場合は空白で分割する", () => {
        expect(parseUsiInput("7g7f 3c3d 2g2f")).toEqual(["7g7f", "3c3d", "2g2f"]);
    });
});
