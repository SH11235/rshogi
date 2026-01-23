declare module "@shogi/engine-wasm/pkg/engine_wasm.js" {
    export function wasm_get_initial_board(): unknown;
    export function wasm_parse_sfen_to_board(sfen: string): unknown;
    export function wasm_board_to_sfen(board_json: unknown): string;
    export function wasm_get_legal_moves(
        sfen: string,
        moves?: unknown,
        pass_rights?: { sente: number; gote: number },
    ): unknown;
    export function wasm_replay_moves_strict(
        sfen: string,
        moves: unknown,
        pass_rights?: { sente: number; gote: number },
    ): unknown;
}
