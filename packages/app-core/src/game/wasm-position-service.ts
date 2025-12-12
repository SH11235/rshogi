import {
    ensureWasmModule,
    wasm_board_to_sfen,
    wasm_get_initial_board,
    wasm_get_legal_moves,
    wasm_parse_sfen_to_board,
    wasm_replay_moves_strict,
} from "@shogi/engine-wasm";
import type { PositionState } from "./board";
import type { BoardStateJson, PositionService, ReplayResult } from "./position-service";
import { boardJsonToPositionState, positionStateToBoardJson } from "./position-service";

export const createWasmPositionService = (): PositionService => {
    let ready: Promise<void> | null = null;
    const ensureReady = () => (ready ??= ensureWasmModule());

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
            const result = wasm_replay_moves_strict(sfen, moves) as {
                applied: string[];
                last_ply: number;
                board: BoardStateJson;
                error?: string;
            };
            return {
                applied: result.applied,
                lastPly: result.last_ply,
                position: toPosition(result.board),
                error: result.error ?? undefined,
            };
        },
    };
};
