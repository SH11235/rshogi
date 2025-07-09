import { describe, expect, it } from "vitest";
import type { Move } from "../../domain/model/move";
import { exportToKif, parseKifMoves, validateKifFormat } from "./kifConverter";

describe("kifService", () => {
    describe("parseKifMoves with initial position", () => {
        it("should parse tsume-shogi KIF with initial position", () => {
            const kifContent = `# KIF形式棋譜ファイル
# 先手の持駒：銀 金
# 後手の持駒：なし
#   ９ ８ ７ ６ ５ ４ ３ ２ １
# +---------------------------+
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|一
# | ・ ・ ・ ・ ・ ・ ・ ・v玉|二
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|三
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|四
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|五
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|六
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|七
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|八
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|九
# +---------------------------+

手数----指手---------消費時間--
   1 ２三銀打     ( 0:00/00:00:00)
   2 １三玉(12)   ( 0:00/00:00:00)
   3 １四金打     ( 0:00/00:00:00)`;

            const result = parseKifMoves(kifContent);

            // 初期局面が設定されていることを確認
            expect(result.initialBoard).toBeDefined();
            expect(result.initialHands).toBeDefined();

            // 持駒を確認
            expect(result.initialHands?.black.銀).toBe(1);
            expect(result.initialHands?.black.金).toBe(1);
            expect(result.initialHands?.white.銀).toBe(0);

            // 手順を確認
            expect(result.moves).toHaveLength(3);
            expect(result.moves[0]).toMatchObject({
                type: "drop",
                to: { row: 3, column: 2 },
                piece: { type: "silver", owner: "black", promoted: false },
            });
        });

        it("should parse normal game without initial position", () => {
            const kifContent = `# KIF形式棋譜ファイル
手数----指手---------消費時間--
   1 ７六歩(77)   ( 0:00/00:00:00)
   2 ３四歩(33)   ( 0:00/00:00:00)`;

            const result = parseKifMoves(kifContent);

            // 初期局面が設定されていないことを確認
            expect(result.initialBoard).toBeUndefined();
            expect(result.initialHands).toBeUndefined();

            // 手順を確認
            expect(result.moves).toHaveLength(2);
        });

        it("should parse board with various pieces", () => {
            const kifContent = `# KIF形式棋譜ファイル
# 先手の持駒：なし
# 後手の持駒：歩
#   ９ ８ ７ ６ ５ ４ ３ ２ １
# +---------------------------+
# |v香v桂v銀v金v玉v金v銀v桂v香|一
# | ・v飛 ・ ・ ・ ・ ・v角 ・|二
# |v歩v歩v歩v歩v歩v歩v歩v歩v歩|三
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|四
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|五
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|六
# | 歩 歩 歩 歩 歩 歩 歩 歩 歩|七
# | ・ 角 ・ ・ ・ ・ ・ 飛 ・|八
# | 香 桂 銀 金 王 金 銀 桂 香|九
# +---------------------------+

手数----指手---------消費時間--`;

            const result = parseKifMoves(kifContent);

            expect(result.initialBoard).toBeDefined();
            expect(result.initialHands).toBeDefined();

            // 後手の持駒に歩が1枚
            expect(result.initialHands?.white.歩).toBe(1);

            // 盤面の駒を確認（一部）
            expect(result.initialBoard).toBeDefined();
            if (result.initialBoard) {
                const board = result.initialBoard;
                expect(board["11"]).toMatchObject({ type: "lance", owner: "white" }); // 1一：後手香車
                expect(board["15"]).toMatchObject({ type: "gyoku", owner: "white" }); // 1五：後手玉
                expect(board["91"]).toMatchObject({ type: "lance", owner: "black" }); // 9一：先手香車
                expect(board["95"]).toMatchObject({ type: "king", owner: "black" }); // 9五：先手王
                expect(board["22"]).toMatchObject({ type: "bishop", owner: "white" }); // 2二：後手角
                expect(board["28"]).toMatchObject({ type: "rook", owner: "white" }); // 2八：後手飛車
            }
        });
    });

    describe("exportToKif", () => {
        it("should export moves to KIF format", () => {
            const moves: Move[] = [
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
                    piece: { type: "silver", owner: "white", promoted: false },
                },
            ];

            const kif = exportToKif(moves);

            expect(kif).toContain("手数----指手---------消費時間--");
            expect(kif).toContain("1 ７六歩");
            expect(kif).toContain("2 ５五銀打");
            expect(kif).toContain("まで2手で対局終了");
        });
    });

    describe("validateKifFormat", () => {
        it("should validate correct KIF format", () => {
            const kif = `# KIF形式棋譜ファイル
手数----指手---------消費時間--
   1 ７六歩(77)   ( 0:00/00:00:00)`;

            const result = validateKifFormat(kif);
            expect(result.valid).toBe(true);
        });

        it("should reject invalid KIF format", () => {
            const kif = "これは無効なKIF形式です";

            const result = validateKifFormat(kif);
            expect(result.valid).toBe(false);
            expect(result.error).toContain("KIF形式のヘッダーが見つかりません");
        });
    });
});
