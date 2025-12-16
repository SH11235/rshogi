import {
    type BoardStateJson,
    boardJsonToPositionState,
    type PositionService,
    type PositionState,
    positionStateToBoardJson,
    type ReplayResult,
    type ReplayResultJson,
} from "@shogi/app-core";
import {
    ensureWasmModule,
    wasm_board_to_sfen,
    wasm_get_initial_board,
    wasm_get_legal_moves,
    wasm_parse_sfen_to_board,
    wasm_replay_moves_strict,
} from "@shogi/engine-wasm";

export const createWasmPositionService = (): PositionService => {
    let ready: Promise<void> | null = null;
    const ensureReady = () => {
        if (!ready) {
            ready = ensureWasmModule();
        }
        return ready;
    };

    const toPosition = (json: BoardStateJson): PositionState => boardJsonToPositionState(json);

    return {
        async getInitialBoard(): Promise<PositionState> {
            await ensureReady();
            const result = wasm_get_initial_board() as BoardStateJson;
            return toPosition(result);
        },

        async parseSfen(sfen: string): Promise<PositionState> {
            await ensureReady();
            const result = wasm_parse_sfen_to_board(sfen) as BoardStateJson;
            return toPosition(result);
        },

        async boardToSfen(position: PositionState): Promise<string> {
            await ensureReady();
            const payload = positionStateToBoardJson(position);
            const boardToSfen = wasm_board_to_sfen as (board: BoardStateJson) => string;
            return boardToSfen(payload);
        },

        async getLegalMoves(sfen: string, moves?: string[]): Promise<string[]> {
            await ensureReady();
            const result = wasm_get_legal_moves(sfen, moves ?? undefined);
            return result as unknown as string[];
        },

        async replayMovesStrict(sfen: string, moves: string[]): Promise<ReplayResult> {
            await ensureReady();
            const result = wasm_replay_moves_strict(sfen, moves) as ReplayResultJson;
            return {
                applied: result.applied,
                lastPly: result.last_ply,
                position: toPosition(result.board),
                error: result.error ?? undefined,
            };
        },
    };
};
