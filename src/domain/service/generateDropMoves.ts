import { type Board, getPiece } from "../model/board";
import type { DropMove } from "../model/move";
import type { Player } from "../model/piece";
import { convertJapanesePieceType, createPiece, isPawn } from "../model/piece";
import type { Column, Row, Square } from "../model/square";
import type { Hands } from "./moveService";

/**
 * Generate possible drop moves for a given player and piece kind.
 * Currently only implements the nifu rule for pawns.
 */
export function generateDropMoves(board: Board, player: Player, japaneseName: string): DropMove[] {
    const moves: DropMove[] = [];
    for (let r = 1 as Row; r <= 9; r++) {
        for (let c = 1 as Column; c <= 9; c++) {
            const square: Square = { row: r as Row, column: c as Column };
            if (getPiece(board, square)) continue;

            // ----- piece specific drop restrictions -----
            if (japaneseName === "歩" || japaneseName === "香") {
                const lastRank = player === "black" ? 1 : 9;
                if (r === lastRank) continue; // cannot drop on last rank
            }
            if (japaneseName === "桂") {
                const last1 = player === "black" ? 1 : 9;
                const last2 = player === "black" ? 2 : 8;
                if (r === last1 || r === last2) continue; // cannot drop on last two ranks
            }

            if (japaneseName === "歩") {
                let blocked = false;
                for (let rr = 1 as Row; rr <= 9; rr++) {
                    const piece = getPiece(board, { row: rr as Row, column: c as Column });
                    if (piece && piece.owner === player && isPawn(piece) && !piece.promoted) {
                        blocked = true;
                        break;
                    }
                }
                if (blocked) continue;
            }

            const pieceType = convertJapanesePieceType(japaneseName);
            moves.push({
                type: "drop",
                to: square,
                piece: createPiece(pieceType, player, false),
            });
        }
    }
    return moves;
}

const handKinds: readonly string[] = ["歩", "香", "桂", "銀", "金", "角", "飛"] as const;

/**
 * Generate drop moves for all pieces currently in the player's hand.
 */
export function generateAllDropMoves(board: Board, hands: Hands, player: Player): DropMove[] {
    const moves: DropMove[] = [];
    const hand = hands[player];
    for (const japaneseName of handKinds) {
        if (hand[japaneseName] > 0) {
            moves.push(...generateDropMoves(board, player, japaneseName));
        }
    }
    return moves;
}
