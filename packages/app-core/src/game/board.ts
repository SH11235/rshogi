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
export type Hand = Partial<Record<PieceType, number>>;
export interface Hands {
    sente: Hand;
    gote: Hand;
}

export type ParsedMove =
    | { kind: "move"; from: Square; to: Square; promote: boolean }
    | { kind: "drop"; to: Square; piece: PieceType };

export interface PositionState {
    board: BoardState;
    hands: Hands;
    turn: Player;
}

export interface LastMove {
    from?: Square | null;
    to: Square;
    dropPiece?: PieceType;
    promotes?: boolean;
}

const ALL_SQUARES: Square[] = BOARD_RANKS.flatMap((rank) =>
    BOARD_FILES.map((file) => `${file}${rank}` as Square),
);

const PIECE_POOL: PieceType[] = ["P", "L", "N", "S", "G", "B", "R", "K"];
const PROMOTED_FROM: Record<PieceType, PieceType> = {
    P: "P",
    L: "L",
    N: "N",
    S: "S",
    G: "G",
    B: "B",
    R: "R",
    K: "K",
};

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

export function createEmptyHands(): Hands {
    return { sente: {}, gote: {} };
}

export function createInitialPositionState(): PositionState {
    return {
        board: createInitialBoard(),
        hands: createEmptyHands(),
        turn: "sente",
    };
}

export function cloneBoard(board: BoardState): BoardState {
    const clone: BoardState = Object.fromEntries(ALL_SQUARES.map((sq) => [sq, null])) as BoardState;
    ALL_SQUARES.forEach((square) => {
        const piece = board[square];
        clone[square] = piece ? { ...piece } : null;
    });
    return clone;
}

function cloneHands(hands: Hands): Hands {
    return {
        sente: { ...hands.sente },
        gote: { ...hands.gote },
    };
}

function toggleTurn(turn: Player): Player {
    return turn === "sente" ? "gote" : "sente";
}

function addToHand(hands: Hands, owner: Player, piece: Piece): Hands {
    const next = cloneHands(hands);
    const key = PROMOTED_FROM[piece.type] ?? piece.type;
    const bucket = next[owner];
    bucket[key] = (bucket[key] ?? 0) + 1;
    return next;
}

function removeFromHand(hands: Hands, owner: Player, piece: PieceType): Hands | null {
    const next = cloneHands(hands);
    const bucket = next[owner];
    const current = bucket[piece] ?? 0;
    if (current <= 0) {
        return null;
    }
    if (current === 1) {
        delete bucket[piece];
    } else {
        bucket[piece] = current - 1;
    }
    return next;
}

function isSquare(value: string): value is Square {
    return ALL_SQUARES.includes(value as Square);
}

interface ApplyOptions {
    validateTurn?: boolean;
    ignoreHandLimits?: boolean;
}

export function applyMove(board: BoardState, move: string, opts: ApplyOptions = {}): BoardState {
    const initialState: PositionState = {
        board,
        hands: createEmptyHands(),
        turn: "sente",
    };
    const result = applyMoveWithState(initialState, move, {
        validateTurn: opts.validateTurn ?? false,
        ignoreHandLimits: opts.ignoreHandLimits ?? true,
    });
    return result.next.board;
}

export function applyMoveWithState(
    state: PositionState,
    move: string,
    opts: ApplyOptions = {},
): { ok: boolean; next: PositionState; lastMove?: LastMove; error?: string } {
    if (!move || move === "resign") {
        return { ok: false, next: state, error: "move is empty or resign" };
    }

    const parsed = parseMove(move);
    if (!parsed) {
        return { ok: false, next: state, error: "invalid move format" };
    }

    const board = cloneBoard(state.board);
    const hands = cloneHands(state.hands);
    const validateTurn = opts.validateTurn ?? true;
    const currentTurn = state.turn;
    let lastMove: LastMove | undefined;

    if (parsed.kind === "drop") {
        if (board[parsed.to]) {
            return { ok: false, next: state, error: "cannot drop onto occupied square" };
        }
        const updatedHands =
            (opts.ignoreHandLimits ?? false)
                ? hands
                : removeFromHand(hands, currentTurn, parsed.piece);
        if (!updatedHands) {
            return { ok: false, next: state, error: "no piece in hand" };
        }
        board[parsed.to] = { owner: currentTurn, type: parsed.piece };
        lastMove = { from: null, to: parsed.to, dropPiece: parsed.piece, promotes: false };
        return {
            ok: true,
            next: { board, hands: updatedHands, turn: toggleTurn(currentTurn) },
            lastMove,
        };
    }

    const fromPiece = board[parsed.from];
    if (!fromPiece) {
        return { ok: false, next: state, error: "no piece at source square" };
    }
    if (validateTurn && fromPiece.owner !== currentTurn) {
        return { ok: false, next: state, error: "not your turn" };
    }
    const targetPiece = board[parsed.to];
    if (targetPiece && targetPiece.owner === fromPiece.owner) {
        return { ok: false, next: state, error: "cannot capture own piece" };
    }

    let updatedHands = hands;
    if (targetPiece) {
        updatedHands = addToHand(hands, fromPiece.owner, targetPiece);
    }

    const movedPiece: Piece = {
        ...fromPiece,
        promoted: parsed.promote ? true : fromPiece.promoted,
    };

    board[parsed.from] = null;
    board[parsed.to] = movedPiece;
    lastMove = { from: parsed.from, to: parsed.to, promotes: parsed.promote };

    return {
        ok: true,
        next: { board, hands: updatedHands, turn: toggleTurn(currentTurn) },
        lastMove,
    };
}

export function replayMoves(
    moves: string[],
    opts: ApplyOptions = {},
): { state: PositionState; errors: string[]; lastMove?: LastMove } {
    let state = createInitialPositionState();
    const errors: string[] = [];
    let lastMove: LastMove | undefined;

    moves.forEach((move, index) => {
        const result = applyMoveWithState(state, move, opts);
        if (!result.ok) {
            errors.push(`move ${index + 1}: ${result.error ?? "unknown error"}`);
            return;
        }
        state = result.next;
        lastMove = result.lastMove;
    });

    return { state, errors, lastMove };
}

export function boardFromMoves(moves: string[]): BoardState {
    const { state } = replayMoves(moves);
    return state.board;
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

export function parseMove(move: string): ParsedMove | null {
    if (!move) return null;
    const dropMatch = move.match(/^([prbsgnlk])\*([1-9][a-i])$/i);
    if (dropMatch) {
        const [, rawPiece, toSquare] = dropMatch;
        const upper = rawPiece.toUpperCase();
        if (!isSquare(toSquare)) return null;
        if (!PIECE_POOL.includes(upper as PieceType)) return null;
        if (upper === "K") return null; // 王を打つ手は存在しないので弾く
        return { kind: "drop", to: toSquare as Square, piece: upper as PieceType };
    }

    const match = move.match(/^([1-9][a-i])([1-9][a-i])(\+)?$/);
    if (!match) return null;
    const [, from, to, promoteFlag] = match;
    if (!isSquare(from) || !isSquare(to)) {
        return null;
    }
    return {
        kind: "move",
        from: from as Square,
        to: to as Square,
        promote: promoteFlag === "+",
    };
}

export function getAllSquares(): Square[] {
    return [...ALL_SQUARES];
}
