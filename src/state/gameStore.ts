import type { Board } from "@/domain/model/board";
import type { Move } from "@/domain/model/move";
import { produce } from "immer";
import { create } from "zustand";
import { devtools, persist, subscribeWithSelector } from "zustand/middleware";

// --- 初期値の仮実装 ---
const initialBoard: Board = {
    /* 後で 初期値入れる */
} as any;

interface GameState {
    board: Board;
    history: Move[];
    cursor: number;
    makeMove: (move: Move) => void;
    undo: () => void;
    redo: () => void;
    reset: () => void;
}

export const useGameStore = create<GameState>()(
    persist(
        devtools(
            subscribeWithSelector((set, _get) => ({
                board: initialBoard,
                history: [],
                cursor: -1,

                makeMove: (move) =>
                    set(
                        produce((s: GameState) => {
                            // TODO domain.applyMove 実装待ち
                            s.history = s.history.slice(0, s.cursor + 1).concat(move);
                            s.cursor++;
                        }),
                        false,
                        "MAKE_MOVE",
                    ),

                undo: () =>
                    set(
                        produce((s) => {
                            if (s.cursor >= 0) s.cursor--;
                        }),
                        false,
                        "UNDO",
                    ),

                redo: () =>
                    set(
                        produce((s) => {
                            if (s.cursor < s.history.length - 1) s.cursor++;
                        }),
                        false,
                        "REDO",
                    ),

                reset: () => set({ board: initialBoard, history: [], cursor: -1 }, false, "RESET"),
            })),
        ),
        { name: "shogi-game", version: 1 },
    ),
);
