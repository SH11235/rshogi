import {
    BOARD_FILES,
    BOARD_RANKS,
    type BoardState,
    type Hands,
    type PassRightsState,
    type Piece,
    type PieceType,
    type Player,
    type PositionState,
    type Square,
} from "./board";

export interface PieceJson {
    owner: Player;
    type: PieceType;
    promoted?: boolean;
}

export interface CellJson {
    square: Square;
    piece: PieceJson | null;
}

export interface HandJson {
    P?: number;
    L?: number;
    N?: number;
    S?: number;
    G?: number;
    B?: number;
    R?: number;
}

export interface HandsJson {
    sente: HandJson;
    gote: HandJson;
}

/**
 * パス権のJSON表現
 */
export interface PassRightsJson {
    sente: number;
    gote: number;
}

export interface BoardStateJson {
    cells: CellJson[][];
    hands: HandsJson;
    turn: Player;
    ply?: number;
    /** パス権の状態（オプション） */
    pass_rights?: PassRightsJson;
}

export interface ReplayResultJson {
    applied: string[];
    last_ply: number;
    board: BoardStateJson;
    error?: string;
}

export interface ReplayResult {
    applied: string[];
    lastPly: number;
    position: PositionState;
    error?: string;
}

export interface PositionService {
    getInitialBoard(): Promise<PositionState>;
    parseSfen(sfen: string): Promise<PositionState>;
    boardToSfen(position: PositionState): Promise<string>;
    getLegalMoves(
        sfen: string,
        moves?: string[],
        options?: { passRights?: { sente: number; gote: number } },
    ): Promise<string[]>;
    replayMovesStrict(
        sfen: string,
        moves: string[],
        options?: { passRights?: { sente: number; gote: number } },
    ): Promise<ReplayResult>;
}

const FILES_ASC: readonly string[] = [...BOARD_FILES].reverse();

const pieceFromJson = (input: PieceJson | null): Piece | null => {
    if (!input) return null;
    const promoted = input.promoted === true;
    return promoted ? { ...input, promoted: true } : { owner: input.owner, type: input.type };
};

const pieceToJson = (piece: Piece | null): PieceJson | null => {
    if (!piece) return null;
    return piece.promoted ? { ...piece, promoted: true } : { owner: piece.owner, type: piece.type };
};

const handsFromJson = (json: HandsJson): Hands => ({
    sente: {
        ...(json.sente.P ? { P: json.sente.P } : {}),
        ...(json.sente.L ? { L: json.sente.L } : {}),
        ...(json.sente.N ? { N: json.sente.N } : {}),
        ...(json.sente.S ? { S: json.sente.S } : {}),
        ...(json.sente.G ? { G: json.sente.G } : {}),
        ...(json.sente.B ? { B: json.sente.B } : {}),
        ...(json.sente.R ? { R: json.sente.R } : {}),
    },
    gote: {
        ...(json.gote.P ? { P: json.gote.P } : {}),
        ...(json.gote.L ? { L: json.gote.L } : {}),
        ...(json.gote.N ? { N: json.gote.N } : {}),
        ...(json.gote.S ? { S: json.gote.S } : {}),
        ...(json.gote.G ? { G: json.gote.G } : {}),
        ...(json.gote.B ? { B: json.gote.B } : {}),
        ...(json.gote.R ? { R: json.gote.R } : {}),
    },
});

const handsToJson = (hands: Hands): HandsJson => ({
    sente: {
        ...(hands.sente.P ? { P: hands.sente.P } : {}),
        ...(hands.sente.L ? { L: hands.sente.L } : {}),
        ...(hands.sente.N ? { N: hands.sente.N } : {}),
        ...(hands.sente.S ? { S: hands.sente.S } : {}),
        ...(hands.sente.G ? { G: hands.sente.G } : {}),
        ...(hands.sente.B ? { B: hands.sente.B } : {}),
        ...(hands.sente.R ? { R: hands.sente.R } : {}),
    },
    gote: {
        ...(hands.gote.P ? { P: hands.gote.P } : {}),
        ...(hands.gote.L ? { L: hands.gote.L } : {}),
        ...(hands.gote.N ? { N: hands.gote.N } : {}),
        ...(hands.gote.S ? { S: hands.gote.S } : {}),
        ...(hands.gote.G ? { G: hands.gote.G } : {}),
        ...(hands.gote.B ? { B: hands.gote.B } : {}),
        ...(hands.gote.R ? { R: hands.gote.R } : {}),
    },
});

export const boardJsonToPositionState = (json: BoardStateJson): PositionState => {
    const board = {} as BoardState;
    for (const row of json.cells) {
        for (const cell of row) {
            board[cell.square] = pieceFromJson(cell.piece);
        }
    }
    const passRights: PassRightsState | undefined = json.pass_rights
        ? { sente: json.pass_rights.sente, gote: json.pass_rights.gote }
        : undefined;
    return {
        board,
        hands: handsFromJson(json.hands),
        turn: json.turn,
        ply: json.ply,
        passRights,
    };
};

export const positionStateToBoardJson = (state: PositionState): BoardStateJson => {
    const cells = BOARD_RANKS.map((rank) =>
        FILES_ASC.map((file) => {
            const square = `${file}${rank}` as Square;
            return {
                square,
                piece: pieceToJson(state.board[square] ?? null),
            };
        }),
    );
    return {
        cells,
        hands: handsToJson(state.hands),
        turn: state.turn,
        ply: state.ply,
        pass_rights: state.passRights
            ? { sente: state.passRights.sente, gote: state.passRights.gote }
            : undefined,
    };
};
