import { invoke } from "@tauri-apps/api/core";
import type { PositionState } from "./board";
import type {
    BoardStateJson,
    PositionService,
    ReplayResult,
    ReplayResultJson,
} from "./position-service";
import { boardJsonToPositionState, positionStateToBoardJson } from "./position-service";

export const createTauriPositionService = (): PositionService => {
    const toPosition = (json: BoardStateJson): PositionState => boardJsonToPositionState(json);

    return {
        async getInitialBoard(): Promise<PositionState> {
            const json = await invoke<BoardStateJson>("get_initial_board");
            return toPosition(json);
        },

        async parseSfen(sfen: string): Promise<PositionState> {
            const json = await invoke<BoardStateJson>("parse_sfen_to_board", { sfen });
            return toPosition(json);
        },

        async boardToSfen(position: PositionState): Promise<string> {
            const payload = positionStateToBoardJson(position);
            return invoke<string>("board_to_sfen", { board: payload });
        },

        async getLegalMoves(sfen: string, moves?: string[]): Promise<string[]> {
            return invoke<string[]>("engine_legal_moves", { sfen, moves });
        },

        async replayMovesStrict(sfen: string, moves: string[]): Promise<ReplayResult> {
            const result = await invoke<ReplayResultJson>("engine_replay_moves_strict", {
                sfen,
                moves,
            });
            return {
                applied: result.applied,
                lastPly: result.last_ply,
                position: toPosition(result.board),
                error: result.error ?? undefined,
            };
        },
    };
};
