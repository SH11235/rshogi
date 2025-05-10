// -------------------------------------------------------
// Pure service functions that manipulate the immutable
// domain model using the Move union type.
// No React / Zustand / I/O dependencies here.
// -------------------------------------------------------

import { type Board, getPiece, setPiece } from "../model/board";
import type { Move, NormalMove } from "../model/move";
import { type Piece, type PieceKind, type Player, promote } from "../model/piece";
import type { Column, Row, Square } from "../model/square";

//--------------------------------------------------------
// Additional domain helpers
//--------------------------------------------------------

/**
 * 持ち駒の所持数を駒種ごとに保持する。
 * 例: hands.black["歩"] === 2 なら先手が歩を 2 枚持っている。
 */
export type Hands = {
    black: Record<PieceKind, number>;
    white: Record<PieceKind, number>;
};

export const createEmptyHands = (): Hands => ({
    black: {
        歩: 0,
        香: 0,
        桂: 0,
        銀: 0,
        金: 0,
        角: 0,
        飛: 0,
        王: 0,
    },
    white: {
        歩: 0,
        香: 0,
        桂: 0,
        銀: 0,
        金: 0,
        角: 0,
        飛: 0,
        王: 0,
    },
});

/** Toggle player side */
export const toggleSide = (side: Player): Player => (side === "black" ? "white" : "black");

//--------------------------------------------------------
// applyMove
//--------------------------------------------------------

export interface ApplyMoveResult {
    board: Board;
    hands: Hands;
    nextTurn: Player;
}

/**
 * immutable applyMove – returns new board/hands/turn
 */
export function applyMove(
    board: Board,
    hands: Hands,
    currentTurn: Player,
    move: Move,
): ApplyMoveResult {
    // deep copy via spreading (Board is shallow Record)
    let newBoard: Board = { ...board };
    const newHands: Hands = JSON.parse(JSON.stringify(hands)) as Hands;

    const mover: Player = currentTurn;
    const opponent: Player = toggleSide(currentTurn);

    if (move.type === "move") {
        // 1. Get and verify source piece
        const srcPiece = getPiece(board, move.from);
        if (!srcPiece) throw new Error("No piece at source square");
        if (srcPiece.owner !== mover) throw new Error("Piece does not belong to mover");

        // 2. Handle capture at destination
        const dstPiece = getPiece(board, move.to);
        if (dstPiece) {
            if (dstPiece.owner === mover) throw new Error("Cannot capture own piece");
            // capture: add to mover's hand, unpromoted
            const unpromoted: PieceKind = dstPiece.kind; // promoted flag ignored when captured
            newHands[mover][unpromoted] += 1;
        }

        // 3. Remove from source
        newBoard = setPiece(newBoard, move.from, null);

        // 4. Apply promote flag
        const placed: Piece = move.promote ? promote(srcPiece) : { ...srcPiece };

        // 5. Place to destination
        newBoard = setPiece(newBoard, move.to, placed);
    } else if (move.type === "drop") {
        // check hand availability
        const kind = move.piece.kind;
        if (newHands[mover][kind] <= 0) throw new Error("No such piece in hand");
        if (getPiece(board, move.to)) throw new Error("Destination not empty");

        // consume hand
        newHands[mover][kind] -= 1;

        // place dropped piece (ownership = mover, promoted=false)
        newBoard = setPiece(newBoard, move.to, { ...move.piece, owner: mover, promoted: false });
    } else {
        const _exhaustive: never = move;
        throw new Error("Unknown move type");
    }

    return { board: newBoard, hands: newHands, nextTurn: opponent };
}

//--------------------------------------------------------
// Undo support (optional but handy for time‑travel)
//--------------------------------------------------------

export function revertMove(
    board: Board,
    hands: Hands,
    currentTurn: Player, // ← after move was applied
    move: Move,
): ApplyMoveResult {
    // reverse: currentTurn is opponent of original mover
    const mover: Player = toggleSide(currentTurn);
    let newBoard: Board = { ...board };
    const newHands: Hands = JSON.parse(JSON.stringify(hands));

    if (move.type === "move") {
        // Move piece back
        const dstPiece = getPiece(board, move.to);
        if (!dstPiece) throw new Error("No piece at destination when undoing");

        // Remove promoted status if it happened in this move
        const originalPiece: Piece = move.promote ? { ...dstPiece, promoted: false } : dstPiece;

        newBoard = setPiece(newBoard, move.to, move.captured); // restore captured piece or null
        newBoard = setPiece(newBoard, move.from, originalPiece);

        // If there was capture, remove it from hand
        if (move.captured) {
            const kind = move.captured.kind;
            newHands[mover][kind] -= 1;
        }
    } else if (move.type === "drop") {
        // remove dropped piece
        newBoard = setPiece(newBoard, move.to, null);
        // give it back to hand
        newHands[mover][move.piece.kind] += 1;
    }

    return { board: newBoard, hands: newHands, nextTurn: mover };
}

//--------------------------------------------------------
// Utility – replay a list of moves from an initial state
//--------------------------------------------------------

type GameSnapshot = { board: Board; hands: Hands; turn: Player };

export function replayMoves(initial: GameSnapshot, moves: Move[]): GameSnapshot {
    return moves.reduce((state, mv) => {
        const { board, hands, nextTurn } = applyMove(state.board, state.hands, state.turn, mv);
        return { board, hands, turn: nextTurn };
    }, initial);
}

/**
 * Generate pseudo‑legal moves (ignore self‑check).
 * If only attack squares are needed, the same list works for isInCheck.
 */
export function generateMoves(board: Board, from: Square): NormalMove[] {
    const piece = getPiece(board, from);
    if (!piece) return [];

    const vectors: [number, number, boolean][] = getMoveVectors(piece); // [dRow, dCol, slide]
    const moves: NormalMove[] = [];

    for (const [dr, dc, slide] of vectors) {
        let r: number = from.row as number;
        let c: number = from.column as number;
        while (true) {
            r += dr;
            c += dc;
            if (r < 1 || r > 9 || c < 1 || c > 9) break; // board bounds
            const targetSquare: Square = { row: r as Row, column: c as Column };
            const targetPiece = getPiece(board, targetSquare);
            if (targetPiece && targetPiece.owner === piece.owner) break; // own piece blocks

            // capture info
            const move: NormalMove = {
                type: "move",
                from,
                to: targetSquare,
                piece,
                promote: false, // promotion handled elsewhere
                captured: targetPiece ?? null,
            };
            moves.push(move);

            if (targetPiece || !slide) break; // stop if captured or non‑slider
            // else continue sliding
        }
    }

    return moves;
}

//--------------------------------------------------------
// Helper: vectors by piece kind/promoted
//--------------------------------------------------------

type Vec = [number, number, boolean]; // dr, dc, slider?

function getMoveVectors(piece: Piece): Vec[] {
    // Direction: black moves "up" (row -1) , white moves "down" (+1)
    const f = piece.owner === "black" ? -1 : 1;
    const vectors: Vec[] = [];
    const add = (dr: number, dc: number, slide = false) => vectors.push([dr, dc, slide]);

    const goldSteps: Vec[] = [
        [f, 0, false],
        [f, -1, false],
        [f, +1, false],
        [0, -1, false],
        [0, +1, false],
        [-f, 0, false],
    ];
    const silverSteps: Vec[] = [
        [f, 0, false],
        [f, -1, false],
        [f, +1, false],
        [-f, -1, false],
        [-f, +1, false],
    ];

    const kingSteps: Vec[] = [
        [1, 0, false],
        [-1, 0, false],
        [0, 1, false],
        [0, -1, false],
        [1, 1, false],
        [1, -1, false],
        [-1, 1, false],
        [-1, -1, false],
    ];

    switch (piece.kind) {
        case "歩":
            if (piece.promoted) vectors.push(...goldSteps);
            else add(f, 0);
            break;
        case "香":
            if (piece.promoted) vectors.push(...goldSteps);
            else add(f, 0, true);
            break;
        case "桂":
            if (piece.promoted) vectors.push(...goldSteps);
            else {
                add(2 * f, -1);
                add(2 * f, +1);
            }
            break;
        case "銀":
            if (piece.promoted) vectors.push(...goldSteps);
            else vectors.push(...silverSteps);
            break;
        case "金":
            vectors.push(...goldSteps);
            break;
        case "角":
            if (piece.promoted) {
                // bishop slides + king orthogonal
                vectors.push([1, 1, true], [1, -1, true], [-1, 1, true], [-1, -1, true]);
                vectors.push(...kingSteps.filter(([dr, dc]) => dr === 0 || dc === 0));
            } else {
                vectors.push([1, 1, true], [1, -1, true], [-1, 1, true], [-1, -1, true]);
            }
            break;
        case "飛":
            if (piece.promoted) {
                // rook slides + king diagonal
                vectors.push([1, 0, true], [-1, 0, true], [0, 1, true], [0, -1, true]);
                vectors.push(
                    ...kingSteps.filter(([dr, dc]) => Math.abs(dr) === 1 && Math.abs(dc) === 1),
                );
            } else {
                vectors.push([1, 0, true], [-1, 0, true], [0, 1, true], [0, -1, true]);
            }
            break;
        case "王":
            vectors.push(...kingSteps);
            break;
    }
    return vectors;
}
