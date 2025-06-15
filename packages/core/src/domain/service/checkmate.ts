import { type Board, getPiece } from "../model/board";
import type { Piece, Player } from "../model/piece";
import { canPromote, isRoyalPiece } from "../model/piece";
import type { Column, Row, Square } from "../model/square";
import { generateAllDropMoves } from "./generateDropMoves";
import { applyMove, generateMoves } from "./moveService";
import type { Hands } from "./moveService";

// 盤上のマスをすべて列挙
const allSquares: Square[] = Array.from({ length: 9 * 9 }, (_, i) => ({
    row: (Math.floor(i / 9) + 1) as Row,
    column: ((i % 9) + 1) as Column,
}));

// 成れるかどうかの位置判定
const canPromoteMove = (piece: Piece, to: Square): boolean => {
    if (!canPromote(piece)) return false;

    const isBlack = piece.owner === "black";
    // 先手は1-3段目、後手は7-9段目で成れる
    return isBlack ? to.row <= 3 : to.row >= 7;
};

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
export const hasAnyLegalMove = (board: Board, hands: Hands, player: Player): boolean => {
    // 1. 盤上の駒の移動を試す
    for (const from of allSquares) {
        const piece = getPiece(board, from);
        if (piece?.owner !== player) continue;

        const moves = generateMoves(board, from);
        for (const move of moves) {
            try {
                // applyMove を使って正確な手の適用（成り処理含む）
                const result = applyMove(board, hands, player, {
                    type: "move",
                    from,
                    to: move.to,
                    piece: move.piece,
                    promote: false, // 通常は成らない手で試す
                    captured: move.captured,
                });

                // 自玉が王手されていなければ合法手あり
                if (!isInCheck(result.board, player)) {
                    return true;
                }
            } catch {
                // エラーの場合は非合法手なのでスキップ
            }

            // 成りも試す（成れる場合）
            if (canPromoteMove(piece, move.to)) {
                try {
                    const result = applyMove(board, hands, player, {
                        type: "move",
                        from,
                        to: move.to,
                        piece: move.piece,
                        promote: true,
                        captured: move.captured,
                    });

                    if (!isInCheck(result.board, player)) {
                        return true;
                    }
                } catch {
                    // エラーの場合は非合法手なのでスキップ
                }
            }
        }
    }

    // 2. 持ち駒の打ち手を試す
    const dropMoves = generateAllDropMoves(board, hands, player);
    for (const drop of dropMoves) {
        try {
            const result = applyMove(board, hands, player, drop);
            if (!isInCheck(result.board, player)) {
                return true;
            }
        } catch {
            // エラーの場合は非合法手なのでスキップ
        }
    }

    return false;
};

// プレイヤーが詰みか？
export const isCheckmate = (board: Board, hands: Hands, player: Player): boolean => {
    return isInCheck(board, player) && !hasAnyLegalMove(board, hands, player);
};
