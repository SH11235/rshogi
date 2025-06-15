import type { Piece } from "./piece";
import type { Square, SquareKey } from "./square";

export type Board = Record<SquareKey, Piece | null>;

export const getPiece = (board: Board, square: Square): Piece | null =>
    board[`${square.row}${square.column}`] || null;

export const setPiece = (board: Board, square: Square, piece: Piece | null): Board => ({
    ...board,
    [`${square.row}${square.column}`]: piece,
});
