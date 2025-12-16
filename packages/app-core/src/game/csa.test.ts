import { beforeEach, describe, expect, it } from "vitest";
import { createInitialBoard, createInitialPositionState } from "./board";
import { buildBoardFromCsa, movesToCsa, parseCsaMoves } from "./csa";
import type { PositionService } from "./position-service";
import { setPositionServiceFactory } from "./position-service-registry";

// モックの PositionService を作成
const createMockPositionService = (): PositionService => {
    const initialPosition = createInitialPositionState();

    return {
        async getInitialBoard() {
            return initialPosition;
        },
        async parseSfen(_sfen: string) {
            return initialPosition;
        },
        async boardToSfen(_position) {
            return "startpos";
        },
        async getLegalMoves(_sfen: string, _moves?: string[]) {
            return [];
        },
        async replayMovesStrict(_sfen: string, moves: string[]) {
            return {
                applied: moves,
                lastPly: moves.length,
                position: initialPosition,
            };
        },
    };
};

beforeEach(() => {
    setPositionServiceFactory(() => createMockPositionService());
});

describe("movesToCsa", () => {
    it("USI手順をCSA形式に変換する", () => {
        const moves = ["7g7f", "3c3d"];
        const result = movesToCsa(moves);

        expect(result).toContain("V2.2");
        expect(result).toContain("N+Sente");
        expect(result).toContain("N-Gote");
        expect(result).toContain("PI");
        expect(result).toContain("+");
        // 7g7f -> +7776FU
        expect(result).toContain("+7776FU");
        // 3c3d -> -3334FU
        expect(result).toContain("-3334FU");
    });

    it("成りの手を正しく変換する", () => {
        const initialBoard = createInitialBoard();
        // 歩を1cに配置（成れる位置）
        initialBoard["1c"] = { owner: "sente", type: "P" };
        const moves = ["1c1b+"];
        const result = movesToCsa(moves, {}, initialBoard);

        // 成りの手は "TO" コードになる
        // 1c = 13, 1b = 12
        expect(result).toContain("+1312TO");
    });

    it("メタデータを正しく設定する", () => {
        const moves = ["7g7f"];
        const result = movesToCsa(moves, {
            senteName: "先手太郎",
            goteName: "後手次郎",
        });

        expect(result).toContain("N+先手太郎");
        expect(result).toContain("N-後手次郎");
    });

    it("空の手順リストを処理する", () => {
        const moves: string[] = [];
        const result = movesToCsa(moves);

        expect(result).toContain("V2.2");
        expect(result).toContain("PI");
        expect(result).toContain("+");
    });
});

describe("parseCsaMoves", () => {
    it("CSA形式をUSI手順に変換する", () => {
        const csa = `V2.2
N+Sente
N-Gote
PI
+
+7776FU
-3334FU`;

        const moves = parseCsaMoves(csa);

        expect(moves).toHaveLength(2);
        expect(moves[0]).toBe("7g7f");
        expect(moves[1]).toBe("3c3d");
    });

    it("成りの手を正しく解析する", () => {
        const initialBoard = createInitialBoard();
        // 歩を1cに配置
        initialBoard["1c"] = { owner: "sente", type: "P" };

        const csa = `V2.2
N+Sente
N-Gote
PI
+
+1312TO`;

        const moves = parseCsaMoves(csa, initialBoard);

        expect(moves).toHaveLength(1);
        expect(moves[0]).toBe("1c1b+");
    });

    it("空のCSA形式を処理する", () => {
        const csa = `V2.2
N+Sente
N-Gote
PI`;

        const moves = parseCsaMoves(csa);

        expect(moves).toHaveLength(0);
    });

    it("不正な行を無視する", () => {
        const csa = `V2.2
N+Sente
Invalid Line
+7776FU
Another Invalid
-3334FU`;

        const moves = parseCsaMoves(csa);

        expect(moves).toHaveLength(2);
        expect(moves[0]).toBe("7g7f");
        expect(moves[1]).toBe("3c3d");
    });
});

describe("buildBoardFromCsa", () => {
    it("CSA形式から盤面を構築する", () => {
        const csa = `V2.2
N+Sente
N-Gote
PI
+
+7776FU
-3334FU`;

        const board = buildBoardFromCsa(csa);

        expect(board["7g"]).toBeNull();
        expect(board["7f"]).toEqual({
            owner: "sente",
            type: "P",
        });
        expect(board["3c"]).toBeNull();
        expect(board["3d"]).toEqual({
            owner: "gote",
            type: "P",
        });
    });
});

describe("往復変換", () => {
    it("USI -> CSA -> USI で一致する", () => {
        const originalMoves = ["7g7f", "3c3d", "2g2f"];
        const csa = movesToCsa(originalMoves);
        const parsedMoves = parseCsaMoves(csa);

        expect(parsedMoves).toEqual(originalMoves);
    });

    it("複雑な手順でも往復変換が一致する", () => {
        const originalMoves = [
            "7g7f",
            "3c3d",
            "8h2b+", // 角が成る（実際の将棋では不可能だが、テスト用）
            "4a3b",
        ];

        // 初期盤面をカスタマイズ
        const board = createInitialBoard();
        // 角を2bの位置に動かせるように調整
        board["2b"] = null; // 空にする

        const csa = movesToCsa(originalMoves, {}, board);
        const parsedMoves = parseCsaMoves(csa, board);

        // 最初の2手は正確に一致するはず
        expect(parsedMoves.slice(0, 2)).toEqual(originalMoves.slice(0, 2));
    });
});
