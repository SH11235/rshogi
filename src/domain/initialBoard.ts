import type { Board } from "@/domain/model/board";
import type { Piece, PieceKind } from "@/domain/model/piece";
import type { Column, Row, SquareKey } from "@/domain/model/square";

const make = (kind: Piece["kind"], owner: Piece["owner"]): Piece => ({
    kind,
    owner,
    promoted: false,
});

/** 行列 → SquareKey 変換ヘルパ */
const key = (row: Row, col: Column): SquareKey => `${row}${col}` as SquareKey;

export const initialBoard: Board = (() => {
    const board = {} as Record<SquareKey, Piece | null>;

    const row9: PieceKind[] = ["香", "桂", "銀", "金", "王", "金", "銀", "桂", "香"];

    // ❶ 先手（black）側
    row9.forEach((kind, i) => {
        board[key(9, (i + 1) as Column)] = make(kind, "black");
    });

    board[key(8, 8)] = make("飛", "black");
    board[key(8, 2)] = make("角", "black");

    for (let c = 1 as Column; c <= 9; c++) {
        board[key(7, c)] = make("歩", "black");
    }

    // ❷ 後手（white）側
    const row1: PieceKind[] = ["香", "桂", "銀", "金", "玉", "金", "銀", "桂", "香"];
    row1.forEach((kind, i) => {
        board[key(1, (i + 1) as Column)] = make(kind, "white");
    });
    board[key(2, 8)] = make("飛", "white");
    board[key(2, 2)] = make("角", "white");
    for (let c = 1 as Column; c <= 9; c++) {
        board[key(3, c)] = make("歩", "white");
    }

    // ❸ 残りマスを null で埋める
    for (let r = 1 as Row; r <= 9; r++)
        for (let c = 1 as Column; c <= 9; c++) board[key(r, c)] ??= null;

    return board as Board;
})();
