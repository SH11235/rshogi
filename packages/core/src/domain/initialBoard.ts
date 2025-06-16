import type { Board } from "./model/board";
import { type Piece, type PieceType, createPiece } from "./model/piece";
import type { Column, Row, SquareKey } from "./model/square";

/** 駒を作成するヘルパー */
const makePiece = (type: PieceType, owner: Piece["owner"]): Piece =>
    createPiece(type, owner, false);

/** 行列 → SquareKey 変換ヘルパ */
const key = (row: Row, col: Column): SquareKey => `${row}${col}` as SquareKey;

export const modernInitialBoard: Board = (() => {
    const board = {} as Record<SquareKey, Piece | null>;

    // 先手（black）の初期配置
    const blackBackRank: PieceType[] = [
        "lance",
        "knight",
        "silver",
        "gold",
        "king",
        "gold",
        "silver",
        "knight",
        "lance",
    ];

    // ❶ 先手（black）側
    blackBackRank.forEach((type, i) => {
        board[key(9, (i + 1) as Column)] = makePiece(type, "black");
    });

    board[key(8, 2)] = makePiece("rook", "black");
    board[key(8, 8)] = makePiece("bishop", "black");

    for (let c = 1 as Column; c <= 9; c++) {
        board[key(7, c)] = makePiece("pawn", "black");
    }

    // ❂ 後手（white）側
    const whiteBackRank: PieceType[] = [
        "lance",
        "knight",
        "silver",
        "gold",
        "gyoku",
        "gold",
        "silver",
        "knight",
        "lance",
    ];
    whiteBackRank.forEach((type, i) => {
        board[key(1, (i + 1) as Column)] = makePiece(type, "white");
    });

    board[key(2, 8)] = makePiece("rook", "white");
    board[key(2, 2)] = makePiece("bishop", "white");

    for (let c = 1 as Column; c <= 9; c++) {
        board[key(3, c)] = makePiece("pawn", "white");
    }

    // ❸ 残りマスを null で埋める
    for (let r = 1 as Row; r <= 9; r++) {
        for (let c = 1 as Column; c <= 9; c++) {
            board[key(r, c)] ??= null;
        }
    }

    return board as Board;
})();
