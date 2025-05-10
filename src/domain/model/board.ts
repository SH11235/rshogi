import { Square } from "./square";
import { Piece } from "./piece";

export type Board = {
    [key: string]: Piece | null;
};

export const getPiece = (board: Board, square: Square): Piece | null =>
    board[`${square.row}${square.column}`] || null;

export const setPiece = (board: Board, square: Square, piece: Piece | null): Board => ({
    ...board,
    [`${square.row}${square.column}`]: piece,
});
