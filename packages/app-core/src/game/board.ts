import { getPositionService } from "./position-service-registry";

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
    | { kind: "drop"; to: Square; piece: PieceType }
    | { kind: "pass" };

/**
 * パス権の状態
 * 各プレイヤーの残りパス権数を保持
 */
export interface PassRightsState {
    /** 先手のパス権残数 */
    sente: number;
    /** 後手のパス権残数 */
    gote: number;
}

export interface PositionState {
    board: BoardState;
    hands: Hands;
    turn: Player;
    ply?: number;
    /**
     * パス権の状態（オプション）
     * 設定されていない場合はパス権ルールが無効
     */
    passRights?: PassRightsState;
}

export interface LastMove {
    from?: Square | null;
    to: Square;
    dropPiece?: PieceType;
    promotes?: boolean;
    /** パス手の場合 true */
    isPass?: boolean;
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

let initialPositionCache: PositionState | null = null;
let initialPositionPromise: Promise<PositionState> | null = null;
let fallbackInitialPosition: PositionState | null = null;

export function createInitialBoard(): BoardState {
    if (initialPositionCache) {
        return cloneBoard(initialPositionCache.board);
    }
    const fallback = getFallbackInitialPosition();
    initialPositionCache = fallback;
    return cloneBoard(fallback.board);
}

export function createEmptyHands(): Hands {
    return { sente: {}, gote: {} };
}

export function createInitialPositionState(): PositionState {
    if (initialPositionCache) {
        return clonePosition(initialPositionCache);
    }
    const fallback = getFallbackInitialPosition();
    initialPositionCache = fallback;
    return clonePosition(fallback);
}

export async function createInitialBoardAsync(): Promise<BoardState> {
    const position = await ensureInitialPosition();
    return cloneBoard(position.board);
}

export async function createInitialPositionStateAsync(): Promise<PositionState> {
    const position = await ensureInitialPosition();
    return clonePosition(position);
}

const clonePosition = (position: PositionState): PositionState => ({
    board: cloneBoard(position.board),
    hands: cloneHands(position.hands),
    turn: position.turn,
    ply: position.ply,
    passRights: position.passRights
        ? { sente: position.passRights.sente, gote: position.passRights.gote }
        : undefined,
});

async function ensureInitialPosition(): Promise<PositionState> {
    if (!initialPositionPromise) {
        initialPositionPromise = (async () => {
            const service = getPositionService();
            const position = await service.getInitialBoard();
            initialPositionCache = position;
            return position;
        })();
    }
    return initialPositionPromise;
}

export function cloneBoard(board: BoardState): BoardState {
    const clone: BoardState = Object.fromEntries(ALL_SQUARES.map((sq) => [sq, null])) as BoardState;
    ALL_SQUARES.forEach((square) => {
        const piece = board[square];
        clone[square] = piece ? { ...piece } : null;
    });
    return clone;
}

const STARTPOS_BOARD_SFEN = "lnsgkgsnl/1r5b1/p1ppppppp/9/9/9/P1PPPPPPP/1B5R1/LNSGKGSNL";

function getFallbackInitialPosition(): PositionState {
    if (fallbackInitialPosition) return fallbackInitialPosition;
    const board = buildBoardFromSfenBoard(STARTPOS_BOARD_SFEN);
    fallbackInitialPosition = {
        board,
        hands: createEmptyHands(),
        turn: "sente",
        ply: 1,
    };
    return fallbackInitialPosition;
}

function buildBoardFromSfenBoard(boardPart: string): BoardState {
    const rows = boardPart.split("/");
    if (rows.length !== 9) {
        throw new Error("invalid startpos board rows");
    }
    const board: BoardState = Object.fromEntries(ALL_SQUARES.map((sq) => [sq, null])) as BoardState;
    rows.forEach((row, rankIdx) => {
        let fileIdx = 0;
        for (let i = 0; i < row.length; i++) {
            const ch = row[i];
            if (/\d/.test(ch)) {
                fileIdx += Number(ch);
                continue;
            }
            const isPromoted = ch === "+";
            const symbol = isPromoted ? row[++i] : ch;
            const owner: Player = symbol === symbol.toLowerCase() ? "gote" : "sente";
            const upper = symbol.toUpperCase();
            const type = upper as PieceType;
            if (!PIECE_POOL.includes(type)) {
                throw new Error(`unknown piece in startpos: ${symbol}`);
            }
            const square = `${BOARD_FILES[fileIdx]}${BOARD_RANKS[rankIdx]}` as Square;
            board[square] = { owner, type, promoted: isPromoted || undefined };
            fileIdx += 1;
        }
        if (fileIdx !== 9) {
            throw new Error("invalid startpos row width");
        }
    });
    return board;
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

    // パス手の処理
    if (parsed.kind === "pass") {
        // パス権が設定されていない場合はエラー
        if (!state.passRights) {
            return { ok: false, next: state, error: "pass rights not enabled" };
        }
        const remainingPassRights = state.passRights[currentTurn];
        if (remainingPassRights <= 0) {
            return { ok: false, next: state, error: "no pass rights remaining" };
        }
        // パス権を消費して手番を交代
        const newPassRights: PassRightsState = {
            sente: currentTurn === "sente" ? state.passRights.sente - 1 : state.passRights.sente,
            gote: currentTurn === "gote" ? state.passRights.gote - 1 : state.passRights.gote,
        };
        // パス手のlastMoveを作成（toは不要だが型の都合上ダミー値を設定）
        lastMove = { isPass: true, to: "5e" as Square };
        return {
            ok: true,
            next: {
                board,
                hands,
                turn: toggleTurn(currentTurn),
                ply: (state.ply ?? 1) + 1,
                passRights: newPassRights,
            },
            lastMove,
        };
    }

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
            next: {
                board,
                hands: updatedHands,
                turn: toggleTurn(currentTurn),
                ply: (state.ply ?? 1) + 1,
                passRights: state.passRights,
            },
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
        next: {
            board,
            hands: updatedHands,
            turn: toggleTurn(currentTurn),
            ply: (state.ply ?? 1) + 1,
            passRights: state.passRights,
        },
        lastMove,
    };
}

export function replayMoves(
    moves: string[],
    opts: ApplyOptions = {},
    initial?: PositionState,
): { state: PositionState; errors: string[]; lastMove?: LastMove } {
    const baseState = initial
        ? clonePosition(initial)
        : initialPositionCache
          ? clonePosition(initialPositionCache)
          : clonePosition(getFallbackInitialPosition());

    let state = baseState;
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

export function boardFromMoves(moves: string[], initial?: PositionState): BoardState {
    const { state } = replayMoves(moves, {}, initial);
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

export function buildPositionString(moves: string[], sfen = "startpos"): string {
    if (!moves.length) {
        return sfen;
    }

    return `${sfen} moves ${moves.join(" ")}`;
}

export function isPlayerPiece(piece: Piece | null, player: Player): boolean {
    return Boolean(piece && piece.owner === player);
}

export function parseMove(move: string): ParsedMove | null {
    if (!move) return null;

    // パス手の判定
    if (move.toLowerCase() === "pass") {
        return { kind: "pass" };
    }

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

/**
 * 指し手がパス手かどうかを判定
 */
export function isPassMove(move: string): boolean {
    return move.toLowerCase() === "pass";
}

/**
 * 現在の局面でパスが可能かどうかを判定
 *
 * パスが可能な条件:
 * 1. パス権ルールが有効（passRightsが設定されている）
 * 2. 手番側のパス権が1以上残っている
 *
 * 注意: 王手されているかどうかはエンジン側で判定される
 * （TypeScript層では王手判定機能がないため）
 */
export function canPass(state: PositionState): boolean {
    if (!state.passRights) {
        return false;
    }
    return state.passRights[state.turn] > 0;
}

export function getAllSquares(): Square[] {
    return [...ALL_SQUARES];
}
