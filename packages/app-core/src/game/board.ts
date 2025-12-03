export const BOARD_FILES = ["9", "8", "7", "6", "5", "4", "3", "2", "1"] as const;
export const BOARD_RANKS = ["a", "b", "c", "d", "e", "f", "g", "h", "i"] as const;

export type File = (typeof BOARD_FILES)[number];
export type Rank = (typeof BOARD_RANKS)[number];
export type Square = `${File}${Rank}`;

export type Player = "sente" | "gote";
export type PieceType = "K" | "R" | "B" | "G" | "S" | "N" | "L" | "P";

export interface Piece {
    owner: Player;
    type: PieceType;
    promoted?: boolean;
}

export type BoardState = Record<Square, Piece | null>;

const ALL_SQUARES: Square[] = BOARD_RANKS.flatMap((rank) =>
    BOARD_FILES.map((file) => `${file}${rank}` as Square),
);

export function createInitialBoard(): BoardState {
    const state: BoardState = Object.fromEntries(ALL_SQUARES.map((sq) => [sq, null])) as BoardState;

    const place = (square: Square, piece: Piece): void => {
        state[square] = piece;
    };

    const sente: Player = "sente";
    const gote: Player = "gote";

    // Gote back rank
    ["9a", "8a", "7a", "6a", "5a", "4a", "3a", "2a", "1a"].forEach((square, index) => {
        const types: PieceType[] = ["L", "N", "S", "G", "K", "G", "S", "N", "L"];
        place(square as Square, { owner: gote, type: types[index] });
    });
    place("2b", { owner: gote, type: "R" });
    place("8b", { owner: gote, type: "B" });
    BOARD_FILES.forEach((file) => {
        place(`${file}c` as Square, { owner: gote, type: "P" });
    });

    // Sente setup
    BOARD_FILES.forEach((file) => {
        place(`${file}g` as Square, { owner: sente, type: "P" });
    });
    place("2h", { owner: sente, type: "B" });
    place("8h", { owner: sente, type: "R" });
    ["9i", "8i", "7i", "6i", "5i", "4i", "3i", "2i", "1i"].forEach((square, index) => {
        const types: PieceType[] = ["L", "N", "S", "G", "K", "G", "S", "N", "L"];
        place(square as Square, { owner: sente, type: types[index] });
    });

    return state;
}

export function cloneBoard(board: BoardState): BoardState {
    const clone: BoardState = Object.fromEntries(ALL_SQUARES.map((sq) => [sq, null])) as BoardState;
    ALL_SQUARES.forEach((square) => {
        const piece = board[square];
        clone[square] = piece ? { ...piece } : null;
    });
    return clone;
}

export function applyMove(board: BoardState, move: string): BoardState {
    // TODO: capture handling, promotion legality, and strict validation are not implemented.
    if (!move || move === "resign") {
        return board;
    }

    const cleanedMove = move.replace("+", "");
    if (cleanedMove.length < 4) {
        return board;
    }

    const from = cleanedMove.slice(0, 2) as Square;
    const to = cleanedMove.slice(2, 4) as Square;

    if (!ALL_SQUARES.includes(from) || !ALL_SQUARES.includes(to)) {
        return board;
    }

    const next = cloneBoard(board);
    const piece = next[from];
    if (!piece) {
        return board;
    }

    const updatedPiece = { ...piece };
    if (move.endsWith("+")) {
        updatedPiece.promoted = true;
    }

    next[from] = null;
    next[to] = updatedPiece;

    return next;
}

export function boardFromMoves(moves: string[]): BoardState {
    let current = createInitialBoard();
    moves.forEach((move) => {
        current = applyMove(current, move);
    });
    return current;
}

export function boardToMatrix(board: BoardState): BoardMatrix {
    return BOARD_RANKS.map((rank) =>
        BOARD_FILES.map((file) => {
            const square = `${file}${rank}` as Square;
            return { square, piece: board[square] };
        }),
    );
}

export type BoardMatrix = Array<Array<{ square: Square; piece: Piece | null }>>;

export function buildPositionString(moves: string[]): string {
    if (!moves.length) {
        return "startpos";
    }

    return `startpos moves ${moves.join(" ")}`;
}

export function isPlayerPiece(piece: Piece | null, player: Player): boolean {
    return Boolean(piece && piece.owner === player);
}

export function parseMove(move: string): { from: Square; to: Square } | null {
    const cleaned = move.replace("+", "");
    if (cleaned.length < 4) {
        return null;
    }
    const from = cleaned.slice(0, 2) as Square;
    const to = cleaned.slice(2, 4) as Square;
    if (!ALL_SQUARES.includes(from) || !ALL_SQUARES.includes(to)) {
        return null;
    }
    return { from, to };
}

export function getAllSquares(): Square[] {
    return [...ALL_SQUARES];
}
