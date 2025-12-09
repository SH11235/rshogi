import {
    applyMove,
    BOARD_FILES,
    BOARD_RANKS,
    type BoardState,
    boardFromMoves,
    cloneBoard,
    createEmptyHands,
    createInitialBoard,
    type Piece,
    type PieceType,
    type Square,
} from "./board";

const RANK_TO_NUMBER: Record<string, string> = Object.fromEntries(
    BOARD_RANKS.map((rank, index) => [rank, String(index + 1)]),
);

const NUMBER_TO_RANK: Record<string, string> = Object.fromEntries(
    BOARD_RANKS.map((rank, index) => [String(index + 1), rank]),
);

const PROMOTED_CODES: Record<PieceType, string> = {
    P: "TO",
    L: "NY",
    N: "NK",
    S: "NG",
    B: "UM",
    R: "RY",
    G: "KI",
    K: "OU",
};

const PIECE_CODES: Record<PieceType, string> = {
    P: "FU",
    L: "KY",
    N: "KE",
    S: "GI",
    G: "KI",
    B: "KA",
    R: "HI",
    K: "OU",
};

const PROMOTED_FROM_CODE: Record<string, PieceType | undefined> = {
    TO: "P",
    NY: "L",
    NK: "N",
    NG: "S",
    UM: "B",
    RY: "R",
};

export interface CsaMetadata {
    senteName?: string;
    goteName?: string;
}

const resolveInitialBoard = (initialBoard?: BoardState): BoardState => {
    if (initialBoard) return cloneBoard(initialBoard);
    try {
        return createInitialBoard();
    } catch (error) {
        throw new Error(
            `初期盤面を取得できませんでした: ${error instanceof Error ? error.message : String(error)}`,
        );
    }
};

export function movesToCsa(
    moves: string[],
    metadata: CsaMetadata = {},
    initialBoard?: BoardState,
): string {
    const lines: string[] = [
        "V2.2",
        `N+${metadata.senteName ?? "Sente"}`,
        `N-${metadata.goteName ?? "Gote"}`,
        "PI",
        "+",
    ];
    let board = resolveInitialBoard(initialBoard);
    moves.forEach((move, index) => {
        const parsed = parseUsiMove(move);
        if (!parsed) {
            console.warn(`Failed to parse USI move at index ${index}: ${move}`);
            return;
        }
        const piece = board[parsed.from];
        if (!piece) {
            return;
        }
        const sign = index % 2 === 0 ? "+" : "-";
        const pieceCode = determinePieceCode(piece, move.endsWith("+"));
        lines.push(`${sign}${toCsaSquare(parsed.from)}${toCsaSquare(parsed.to)}${pieceCode}`);
        board = applyMove(board, move);
    });

    return lines.join("\n");
}

export function parseCsaMoves(contents: string, initialBoard?: BoardState): string[] {
    const lines = contents
        .split(/\r?\n/)
        .map((line) => line.trim())
        .filter(Boolean);
    const moves: string[] = [];
    let board = resolveInitialBoard(initialBoard);
    for (const line of lines) {
        if (!(line.startsWith("+") || line.startsWith("-"))) {
            continue;
        }
        if (line.length < 7) {
            continue;
        }
        const fromSquare = fromCsaSquare(line.slice(1, 3));
        const toSquare = fromCsaSquare(line.slice(3, 5));
        if (!fromSquare || !toSquare) {
            continue;
        }
        const pieceCode = line.slice(5, 7).toUpperCase();
        const targetPiece = board[fromSquare];
        if (!targetPiece) {
            continue;
        }
        const promotes = PROMOTED_FROM_CODE[pieceCode] !== undefined;
        const move = `${fromSquare}${toSquare}${promotes ? "+" : ""}`;
        moves.push(move);
        board = applyMove(board, move);
    }
    return moves;
}

export function buildBoardFromCsa(contents: string, initialBoard?: BoardState): BoardState {
    const moves = parseCsaMoves(contents, initialBoard);
    const start = resolveInitialBoard(initialBoard);
    return boardFromMoves(moves, { board: start, hands: createEmptyHands(), turn: "sente" });
}

function toCsaSquare(square: Square): string {
    const file = square[0];
    const rank = square[1];
    return `${file}${RANK_TO_NUMBER[rank]}`;
}

function fromCsaSquare(value: string): Square | null {
    if (value.length !== 2) {
        return null;
    }
    const [file, rank] = value.split("");
    if (!BOARD_FILES.includes(file as (typeof BOARD_FILES)[number])) {
        return null;
    }
    const mappedRank = NUMBER_TO_RANK[rank];
    if (!mappedRank) {
        return null;
    }
    return `${file}${mappedRank}` as Square;
}

function parseUsiMove(move: string): { from: Square; to: Square } | null {
    const cleaned = move.replace("+", "");
    if (cleaned.length < 4) {
        return null;
    }
    const from = cleaned.slice(0, 2) as Square;
    const to = cleaned.slice(2, 4) as Square;
    return { from, to };
}

function determinePieceCode(piece: Piece, promoted: boolean): string {
    if (promoted) {
        return PROMOTED_CODES[piece.type] ?? PIECE_CODES[piece.type];
    }
    return PIECE_CODES[piece.type];
}
