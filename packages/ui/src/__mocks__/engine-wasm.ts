// Mock for @shogi/engine-wasm/pkg/engine_wasm.js
// This mock is used in tests to avoid loading the actual WASM module

export const wasm_get_initial_board = () => ({});
export const wasm_get_legal_moves = () => [];
export const wasm_apply_move = () => ({});
export const wasm_board_to_sfen = () => "startpos";
export const wasm_parse_sfen_to_board = () => ({});
export const wasm_replay_moves_strict = () => ({});
