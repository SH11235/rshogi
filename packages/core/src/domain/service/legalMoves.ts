import { type Board, getPiece } from "../model/board";
import type { Piece, PieceType, Player } from "../model/piece";
import { canPromote } from "../model/piece";
import type { Square } from "../model/square";
import { isInCheck } from "./checkmate";
import { generateAllDropMoves } from "./generateDropMoves";
import { applyMove, generateMoves } from "./moveService";
import type { Hands } from "./moveService";

// 成れるかどうかの位置判定
const canPromoteMove = (piece: Piece, to: Square): boolean => {
    if (!canPromote(piece)) return false;

    const isBlack = piece.owner === "black";
    // 先手は1-3段目、後手は7-9段目で成れる
    return isBlack ? to.row <= 3 : to.row >= 7;
};

/**
 * 指定した駒の合法手を生成（王手放置チェック含む）
 * @param board 現在の盤面
 * @param hands 現在の持ち駒
 * @param square 駒の位置
 * @param player プレイヤー
 * @returns 合法手の配列
 */
export const generateLegalMoves = (
    board: Board,
    hands: Hands,
    square: Square,
    player: Player,
): Square[] => {
    const piece = getPiece(board, square);
    if (!piece || piece.owner !== player) {
        return [];
    }

    const moves = generateMoves(board, square);
    const legalMoves: Square[] = [];

    for (const move of moves) {
        // 成らない手を試す
        try {
            const result = applyMove(board, hands, player, {
                type: "move",
                from: square,
                to: move.to,
                piece: move.piece,
                promote: false,
                captured: move.captured,
            });

            // 自玉が王手されていなければ合法手
            if (!isInCheck(result.board, player)) {
                legalMoves.push(move.to);
            }
        } catch {
            // エラーの場合は非合法手なのでスキップ
        }

        // 成る手も試す（成れる場合）
        if (canPromoteMove(piece, move.to)) {
            try {
                const result = applyMove(board, hands, player, {
                    type: "move",
                    from: square,
                    to: move.to,
                    piece: move.piece,
                    promote: true,
                    captured: move.captured,
                });

                if (!isInCheck(result.board, player)) {
                    // 成る手は別途扱わないため、既に追加済みでなければ追加
                    if (
                        !legalMoves.some(
                            (sq) => sq.row === move.to.row && sq.column === move.to.column,
                        )
                    ) {
                        legalMoves.push(move.to);
                    }
                }
            } catch {
                // エラーの場合は非合法手なのでスキップ
            }
        }
    }

    return legalMoves;
};

/**
 * 持ち駒の合法なドロップ先を生成（王手放置チェック含む）
 * @param board 現在の盤面
 * @param hands 現在の持ち駒
 * @param player プレイヤー
 * @returns 合法なドロップ先の配列
 */
export const generateLegalDropMoves = (board: Board, hands: Hands, player: Player): Square[] => {
    const dropMoves = generateAllDropMoves(board, hands, player);
    const legalDrops: Square[] = [];

    for (const drop of dropMoves) {
        try {
            const result = applyMove(board, hands, player, drop);
            if (!isInCheck(result.board, player)) {
                legalDrops.push(drop.to);
            }
        } catch {
            // エラーの場合は非合法手なのでスキップ
        }
    }

    return legalDrops;
};

/**
 * 特定の駒種類の合法なドロップ先を生成
 * @param board 現在の盤面
 * @param hands 現在の持ち駒
 * @param pieceType 駒の種類
 * @param player プレイヤー
 * @returns 合法なドロップ先の配列
 */
export const generateLegalDropMovesForPiece = (
    board: Board,
    hands: Hands,
    pieceType: PieceType,
    player: Player,
): Square[] => {
    const allDrops = generateAllDropMoves(board, hands, player);
    const pieceDrops = allDrops.filter((drop) => drop.piece.type === pieceType);
    const legalDrops: Square[] = [];

    for (const drop of pieceDrops) {
        try {
            const result = applyMove(board, hands, player, drop);
            if (!isInCheck(result.board, player)) {
                legalDrops.push(drop.to);
            }
        } catch {
            // エラーの場合は非合法手なのでスキップ
        }
    }

    return legalDrops;
};
