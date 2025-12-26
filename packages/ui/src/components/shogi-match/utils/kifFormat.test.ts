import type { BoardState, Piece, PieceType, Square } from "@shogi/app-core";
import { describe, expect, it } from "vitest";
import {
    convertMovesToKif,
    evalToY,
    exportToKifString,
    formatEval,
    formatMoveSimple,
    formatMoveToKif,
    getPieceName,
    parseToSquare,
    squareToKanji,
} from "./kifFormat";

// テスト用のヘルパー: 空の盤面を作成
function createEmptyBoard(): BoardState {
    const squares: Square[] = [];
    for (let file = 1; file <= 9; file++) {
        for (const rank of ["a", "b", "c", "d", "e", "f", "g", "h", "i"]) {
            squares.push(`${file}${rank}` as Square);
        }
    }
    return Object.fromEntries(squares.map((sq) => [sq, null])) as BoardState;
}

// テスト用のヘルパー: 駒を配置
function placePiece(
    board: BoardState,
    square: Square,
    owner: "sente" | "gote",
    type: PieceType,
    promoted?: boolean,
): BoardState {
    return {
        ...board,
        [square]: { owner, type, promoted } as Piece,
    };
}

describe("squareToKanji", () => {
    it("5e を ５五 に変換する", () => {
        expect(squareToKanji("5e")).toBe("５五");
    });

    it("7g を ７七 に変換する", () => {
        expect(squareToKanji("7g")).toBe("７七");
    });

    it("1a を １一 に変換する", () => {
        expect(squareToKanji("1a")).toBe("１一");
    });

    it("9i を ９九 に変換する", () => {
        expect(squareToKanji("9i")).toBe("９九");
    });

    it("7f を ７六 に変換する", () => {
        expect(squareToKanji("7f")).toBe("７六");
    });
});

describe("parseToSquare", () => {
    it("通常移動から移動先を取得する", () => {
        expect(parseToSquare("7g7f")).toBe("7f");
        expect(parseToSquare("3c3d")).toBe("3d");
    });

    it("成り移動から移動先を取得する", () => {
        expect(parseToSquare("8h2b+")).toBe("2b");
    });

    it("駒打ちから移動先を取得する", () => {
        expect(parseToSquare("P*5e")).toBe("5e");
        expect(parseToSquare("G*4b")).toBe("4b");
    });

    it("無効な入力には undefined を返す", () => {
        expect(parseToSquare("")).toBeUndefined();
        expect(parseToSquare("7g")).toBeUndefined();
    });
});

describe("getPieceName", () => {
    it("歩の名前を取得する", () => {
        expect(getPieceName("P", false)).toBe("歩");
        expect(getPieceName("P", true)).toBe("と");
    });

    it("香の名前を取得する", () => {
        expect(getPieceName("L", false)).toBe("香");
        expect(getPieceName("L", true)).toBe("成香");
    });

    it("桂の名前を取得する", () => {
        expect(getPieceName("N", false)).toBe("桂");
        expect(getPieceName("N", true)).toBe("成桂");
    });

    it("銀の名前を取得する", () => {
        expect(getPieceName("S", false)).toBe("銀");
        expect(getPieceName("S", true)).toBe("成銀");
    });

    it("金の名前を取得する", () => {
        expect(getPieceName("G", false)).toBe("金");
    });

    it("角の名前を取得する", () => {
        expect(getPieceName("B", false)).toBe("角");
        expect(getPieceName("B", true)).toBe("馬");
    });

    it("飛の名前を取得する", () => {
        expect(getPieceName("R", false)).toBe("飛");
        expect(getPieceName("R", true)).toBe("龍");
    });

    it("玉の名前を取得する", () => {
        expect(getPieceName("K", false)).toBe("玉");
    });
});

describe("formatMoveToKif", () => {
    it("先手の歩の移動を変換する", () => {
        const board = placePiece(createEmptyBoard(), "7g", "sente", "P");
        expect(formatMoveToKif("7g7f", "sente", board)).toBe("▲７六歩(77)");
    });

    it("後手の歩の移動を変換する", () => {
        const board = placePiece(createEmptyBoard(), "3c", "gote", "P");
        expect(formatMoveToKif("3c3d", "gote", board)).toBe("△３四歩(33)");
    });

    it("成り移動を変換する", () => {
        const board = placePiece(createEmptyBoard(), "8h", "sente", "B");
        expect(formatMoveToKif("8h2b+", "sente", board)).toBe("▲２二角成(88)");
    });

    it("駒打ちを変換する", () => {
        const board = createEmptyBoard();
        expect(formatMoveToKif("P*5e", "sente", board)).toBe("▲５五歩打");
        expect(formatMoveToKif("G*4b", "gote", board)).toBe("△４二金打");
    });

    it("同の表記を使用する", () => {
        // 直前の手が7fへの移動で、今回も7fへ移動する場合（駒を取る）
        const board = placePiece(createEmptyBoard(), "8f", "gote", "P");
        // 直前の手が7fへの移動だった場合、今回7fへ移動すると「同」になる
        expect(formatMoveToKif("8f7f", "gote", board, "7f" as Square)).toBe("△同　歩(86)");
    });

    it("成り駒の移動を変換する", () => {
        const board = placePiece(createEmptyBoard(), "5e", "sente", "B", true);
        expect(formatMoveToKif("5e4d", "sente", board)).toBe("▲４四馬(55)");
    });

    it("盤面に駒がない場合はUSI形式をそのまま返す", () => {
        const board = createEmptyBoard();
        expect(formatMoveToKif("7g7f", "sente", board)).toBe("▲7g7f");
    });
});

describe("formatMoveSimple", () => {
    it("先手の歩の移動を変換する（☗記号、半角数字）", () => {
        const board = placePiece(createEmptyBoard(), "7g", "sente", "P");
        expect(formatMoveSimple("7g7f", "sente", board)).toBe("☗7六歩(77)");
    });

    it("後手の歩の移動を変換する（☖記号、半角数字）", () => {
        const board = placePiece(createEmptyBoard(), "3c", "gote", "P");
        expect(formatMoveSimple("3c3d", "gote", board)).toBe("☖3四歩(33)");
    });

    it("成り移動を変換する", () => {
        const board = placePiece(createEmptyBoard(), "8h", "sente", "B");
        expect(formatMoveSimple("8h2b+", "sente", board)).toBe("☗2二角成(88)");
    });

    it("駒打ちを変換する", () => {
        const board = createEmptyBoard();
        expect(formatMoveSimple("P*5e", "sente", board)).toBe("☗5五歩打");
        expect(formatMoveSimple("G*4b", "gote", board)).toBe("☖4二金打");
    });

    it("同の表記を使用する", () => {
        const board = placePiece(createEmptyBoard(), "8f", "gote", "P");
        expect(formatMoveSimple("8f7f", "gote", board, "7f" as Square)).toBe("☖同　歩(86)");
    });

    it("成り駒の移動を変換する", () => {
        const board = placePiece(createEmptyBoard(), "5e", "sente", "B", true);
        expect(formatMoveSimple("5e4d", "sente", board)).toBe("☗4四馬(55)");
    });

    it("盤面に駒がない場合はUSI形式をそのまま返す", () => {
        const board = createEmptyBoard();
        expect(formatMoveSimple("7g7f", "sente", board)).toBe("☗7g7f");
    });
});

describe("formatEval", () => {
    it("正の評価値をフォーマットする", () => {
        expect(formatEval(50)).toBe("+0.5");
        expect(formatEval(150)).toBe("+1.5");
        expect(formatEval(1000)).toBe("+10.0");
    });

    it("負の評価値をフォーマットする", () => {
        expect(formatEval(-50)).toBe("-0.5");
        expect(formatEval(-150)).toBe("-1.5");
    });

    it("0の評価値をフォーマットする", () => {
        expect(formatEval(0)).toBe("+0.0");
    });

    it("先手の詰みをフォーマットする（ply=1, 奇数=先手の手後）", () => {
        // ply=1（先手が指した後）でevalMate > 0 → 先手が詰ませられる
        expect(formatEval(undefined, 3, 1)).toBe("☗詰3");
        expect(formatEval(undefined, 1, 1)).toBe("☗詰1");
    });

    it("後手の詰みをフォーマットする（ply=2, 偶数=後手の手後）", () => {
        // ply=2（後手が指した後）でevalMate > 0 → 後手が詰ませられる
        expect(formatEval(undefined, 3, 2)).toBe("☖詰3");
        expect(formatEval(undefined, 1, 2)).toBe("☖詰1");
    });

    it("先手が詰まされる場合（先手の手後で負の詰み）", () => {
        // ply=1（先手が指した後）でevalMate < 0 → 先手が詰まされる = 後手が詰ませられる
        expect(formatEval(undefined, -3, 1)).toBe("☖詰3");
        expect(formatEval(undefined, -1, 1)).toBe("☖詰1");
    });

    it("後手が詰まされる場合（後手の手後で負の詰み）", () => {
        // ply=2（後手が指した後）でevalMate < 0 → 後手が詰まされる = 先手が詰ませられる
        expect(formatEval(undefined, -3, 2)).toBe("☗詰3");
        expect(formatEval(undefined, -1, 2)).toBe("☗詰1");
    });

    it("plyがない場合は記号なしで詰みを表示（後方互換性）", () => {
        // plyがundefinedの場合、記号は☗になる（奇数扱い）
        expect(formatEval(undefined, 3)).toBe("☗詰3");
        expect(formatEval(undefined, -3)).toBe("☖詰3");
    });

    it("詰みが評価値より優先される", () => {
        expect(formatEval(100, 5, 1)).toBe("☗詰5");
    });

    it("undefined の場合は空文字を返す", () => {
        expect(formatEval(undefined, undefined)).toBe("");
        expect(formatEval()).toBe("");
    });
});

describe("evalToY", () => {
    const height = 100;

    it("0の評価値は中央に位置する", () => {
        expect(evalToY(0, null, height)).toBe(50);
    });

    it("正の評価値は上半分に位置する", () => {
        const y = evalToY(1000, null, height);
        expect(y).toBeLessThan(50);
        expect(y).toBeGreaterThan(0);
    });

    it("負の評価値は下半分に位置する", () => {
        const y = evalToY(-1000, null, height);
        expect(y).toBeGreaterThan(50);
        expect(y).toBeLessThan(100);
    });

    it("最大評価値は上端付近に位置する", () => {
        const y = evalToY(2000, null, height);
        expect(y).toBeLessThanOrEqual(10);
    });

    it("最小評価値は下端付近に位置する", () => {
        const y = evalToY(-2000, null, height);
        expect(y).toBeGreaterThanOrEqual(90);
    });

    it("詰み（勝ち）は上端付近に位置する", () => {
        const y = evalToY(null, 3, height);
        expect(y).toBe(4);
    });

    it("詰み（負け）は下端付近に位置する", () => {
        const y = evalToY(null, -3, height);
        expect(y).toBe(96);
    });

    it("未計算は中央に位置する", () => {
        expect(evalToY(null, null, height)).toBe(50);
        expect(evalToY(undefined, undefined, height)).toBe(50);
    });
});

describe("convertMovesToKif", () => {
    it("複数の指し手を変換する", () => {
        const board1 = placePiece(createEmptyBoard(), "7g", "sente", "P");
        const board2 = placePiece(createEmptyBoard(), "3c", "gote", "P");
        const board3 = placePiece(createEmptyBoard(), "2g", "sente", "P");

        const moves = ["7g7f", "3c3d", "2g2f"];
        const boardHistory = [board1, board2, board3];

        const result = convertMovesToKif(moves, boardHistory);

        expect(result).toHaveLength(3);
        expect(result[0]).toMatchObject({
            ply: 1,
            kifText: "▲７六歩(77)",
            displayText: "☗7六歩(77)",
            usiMove: "7g7f",
        });
        expect(result[1]).toMatchObject({
            ply: 2,
            kifText: "△３四歩(33)",
            displayText: "☖3四歩(33)",
            usiMove: "3c3d",
        });
        expect(result[2]).toMatchObject({
            ply: 3,
            kifText: "▲２六歩(27)",
            displayText: "☗2六歩(27)",
            usiMove: "2g2f",
        });
    });

    it("評価値を含めて変換する", () => {
        const board1 = placePiece(createEmptyBoard(), "7g", "sente", "P");

        const moves = ["7g7f"];
        const boardHistory = [board1];
        const evalMap = new Map([[1, { scoreCp: 50, depth: 20 }]]);

        const result = convertMovesToKif(moves, boardHistory, evalMap);

        expect(result[0]).toMatchObject({
            ply: 1,
            kifText: "▲７六歩(77)",
            displayText: "☗7六歩(77)",
            evalCp: 50,
            depth: 20,
        });
    });

    it("同の表記を正しく使用する", () => {
        // 7g7f の後に同じ7f に駒が移動するケース
        const board1 = placePiece(createEmptyBoard(), "7g", "sente", "P");
        const board2 = placePiece(createEmptyBoard(), "7f", "gote", "P"); // 7fに歩がある状態

        const moves = ["7g7f", "7f7e"]; // ←実際には不自然な手順だがテスト用
        const boardHistory = [board1, board2];

        const result = convertMovesToKif(moves, boardHistory);

        expect(result[0].kifText).toBe("▲７六歩(77)");
        expect(result[0].displayText).toBe("☗7六歩(77)");
        // 直前の移動先は7fなので、7f からの移動は「同」にならない
        // （移動先が同じ場合のみ「同」になる）
        expect(result[1].kifText).toBe("△７五歩(76)");
        expect(result[1].displayText).toBe("☖7五歩(76)");
    });
});

describe("exportToKifString", () => {
    const buildSingleMoveExport = (startSfen?: string) => {
        const board = placePiece(createEmptyBoard(), "7g", "sente", "P");
        const kifMoves = [
            {
                ply: 1,
                kifText: "dummy",
                displayText: "dummy",
                usiMove: "7g7f",
                elapsedMs: 0,
            },
        ];
        return exportToKifString(kifMoves, [board], { startSfen });
    };

    it("開始局面が平手の場合は開始局面行を省略する", () => {
        const result = buildSingleMoveExport("startpos");
        expect(result).not.toContain("開始局面：");
    });

    it("開始局面が平手SFENの場合は開始局面行を省略する", () => {
        const result = buildSingleMoveExport(
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        );
        expect(result).not.toContain("開始局面：");
    });

    it("開始局面が平手以外なら開始局面行を出力する", () => {
        const result = buildSingleMoveExport(
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPP1/1B5R1/LNSGKGSNL b - 1",
        );
        expect(result).toContain(
            "開始局面：lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPP1/1B5R1/LNSGKGSNL b - 1",
        );
    });
});
