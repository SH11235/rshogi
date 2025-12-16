// Mock for ../pkg/engine_wasm.js
// This mock is used in tests to avoid loading the actual WASM module

import { vi } from "vitest";

// Mock the WASM module initialization
const mockDefault = vi.fn().mockResolvedValue({});
export default mockDefault;

// Mock WASM functions
export const wasm_get_initial_board = vi.fn().mockReturnValue({});
export const wasm_parse_sfen_to_board = vi.fn().mockReturnValue({});
export const wasm_board_to_sfen = vi.fn().mockReturnValue("startpos");
export const wasm_get_legal_moves = vi.fn().mockReturnValue([]);
export const wasm_replay_moves_strict = vi.fn().mockReturnValue({
    applied: [],
    last_ply: 0,
    board: {},
});
