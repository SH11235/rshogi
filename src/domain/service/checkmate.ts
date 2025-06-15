import { type Board, getPiece, setPiece } from "../model/board";
import type { Player } from "../model/piece";
import { isRoyalPiece } from "../model/piece";
import type { Column, Row, Square } from "../model/square";
import { generateMoves } from "./moveService";

// 盤上のマスをすべて列挙
const allSquares: Square[] = Array.from({ length: 9 * 9 }, (_, i) => ({
    row: (Math.floor(i / 9) + 1) as Row,
    column: ((i % 9) + 1) as Column,
}));

// 王または玉の位置を取得
const findKingSquare = (board: Board, player: Player): Square | null => {
    return (
        allSquares.find((sq) => {
            const piece = getPiece(board, sq);
            return piece && isRoyalPiece(piece) && piece.owner === player;
        }) || null
    );
};

// 指定プレイヤーが王手されているかどうか
export const isInCheck = (board: Board, player: Player): boolean => {
    const kingSquare = findKingSquare(board, player);
    if (!kingSquare) return true; // 王がいない = 詰みとみなす

    for (const square of allSquares) {
        const piece = getPiece(board, square);
        if (piece && piece.owner !== player) {
            const moves = generateMoves(board, square);
            if (
                moves.some((m) => m.to.row === kingSquare.row && m.to.column === kingSquare.column)
            ) {
                return true;
            }
        }
    }
    return false;
};

// 指定プレイヤーの合法手が1つもないか調べる（詰み判定に使用）
const hasAnyLegalMove = (board: Board, player: Player): boolean => {
    for (const from of allSquares) {
        const piece = getPiece(board, from);
        if (piece?.owner !== player) continue;

        const moves = generateMoves(board, from);
        for (const move of moves) {
            const nextBoard = simulateMove(board, move.from, move.to);
            if (!isInCheck(nextBoard, player)) {
                return true;
            }
        }
    }
    return false;
};

// 一手進めた仮想盤面を作成（純粋関数）
const simulateMove = (board: Board, from: Square, to: Square): Board => {
    const piece = getPiece(board, from);
    const newBoard = setPiece(board, from, null);
    return setPiece(newBoard, to, piece);
};

// プレイヤーが詰みか？
export const isCheckmate = (board: Board, player: Player): boolean => {
    return isInCheck(board, player) && !hasAnyLegalMove(board, player);
};
