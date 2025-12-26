import { describe, expect, it } from "vitest";
import { parseKif, parseSfen } from "./kifParser";

const HIRATE_SFEN = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

describe("parseSfen", () => {
    it("startpos を平手SFENに正規化する", () => {
        expect(parseSfen("startpos")).toEqual({ sfen: HIRATE_SFEN, moves: [] });
    });

    it("sfen キーワードを除去する", () => {
        expect(parseSfen(`sfen ${HIRATE_SFEN}`)).toEqual({ sfen: HIRATE_SFEN, moves: [] });
    });

    it("position startpos moves を分離する", () => {
        expect(parseSfen("position startpos moves 7g7f 3c3d")).toEqual({
            sfen: HIRATE_SFEN,
            moves: ["7g7f", "3c3d"],
        });
    });
});

describe("parseKif", () => {
    it("開始局面行からSFENを取得する", () => {
        const kif = [
            "#KIF version=2.0 encoding=UTF-8",
            `開始局面：sfen ${HIRATE_SFEN}`,
            "手数----指手---------消費時間--",
            "   1 ７六歩(77)",
        ].join("\n");

        const result = parseKif(kif);

        expect(result.success).toBe(true);
        expect(result.startSfen).toBe(HIRATE_SFEN);
        expect(result.moves).toEqual(["7g7f"]);
    });

    it("開始局面がstartposの場合は平手SFENに正規化する", () => {
        const kif = ["開始局面：startpos", "1 ７六歩(77)"].join("\n");
        const result = parseKif(kif);
        expect(result.success).toBe(true);
        expect(result.startSfen).toBe(HIRATE_SFEN);
    });

    it("開始局面が平手表記の場合は平手SFENに正規化する", () => {
        const kif = ["開始局面：平手", "1 ７六歩(77)"].join("\n");
        const result = parseKif(kif);
        expect(result.success).toBe(true);
        expect(result.startSfen).toBe(HIRATE_SFEN);
    });

    it("開始局面行が無い場合はundefined", () => {
        const kif = ["1 ７六歩(77)"].join("\n");
        const result = parseKif(kif);
        expect(result.success).toBe(true);
        expect(result.startSfen).toBeUndefined();
    });
});
