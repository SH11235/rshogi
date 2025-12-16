// Mock for ../pkg/engine_wasm.js
// This mock is used in tests to avoid loading the actual WASM module

import { vi } from "vitest";

// Mock the WASM module initialization
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const mockDefault: any = vi.fn().mockResolvedValue(undefined);
export default mockDefault;

// Mock WASM functions
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const wasm_get_initial_board: any = vi.fn().mockReturnValue({});
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const wasm_parse_sfen_to_board: any = vi.fn().mockReturnValue({});
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const wasm_board_to_sfen: any = vi.fn().mockReturnValue("startpos");
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const wasm_get_legal_moves: any = vi.fn().mockReturnValue([]);
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const wasm_replay_moves_strict: any = vi.fn().mockReturnValue({
    applied: [],
    last_ply: 0,
    board: {},
});
