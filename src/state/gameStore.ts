import { initialBoard } from "@/domain/initialBoard";
import type { Board } from "@/domain/model/board";
import type { Move } from "@/domain/model/move";
import type { HandKind, Player } from "@/domain/model/piece";
import { applyMove, createEmptyHands, replayMoves } from "@/domain/service/moveService";
import { produce } from "immer";
import { create } from "zustand";
import { devtools, persist, subscribeWithSelector } from "zustand/middleware";

interface GameResult {
    winner: Player | null; // 勝者（null は未決定）
    reason: "checkmate" | "timeout" | "resign"; // 勝因（例：王手放置: checkmate、持ち時間切れ: timeout、投了: resign）
}

interface GameState {
    board: Board; // 現在の盤面
    history: Move[]; // 棋譜（指し手の履歴）
    cursor: number; // 現在の指し手のインデックス（-1 は初期状態）
    turn: "black" | "white"; // 現在の手番
    hands: { black: Record<HandKind, number>; white: Record<HandKind, number> }; // 持ち駒の数（黒・白それぞれ）
    result: GameResult | null; // ゲームの結果（勝者、勝因など）
    makeMove: (move: Move) => void; // 1 手指すアクション。history を切り詰めて追加し、cursor++ する
    undo: () => void; // 1 手戻すアクション。cursor-- する
    redo: () => void; // 1 手進めるアクション。cursor++ する
    reset: () => void; // 盤面を初期化するアクション
}

const initialState: Omit<GameState, "makeMove" | "undo" | "redo" | "reset"> = {
    board: initialBoard,
    history: [],
    cursor: -1,
    turn: "black",
    hands: createEmptyHands(),
    result: null,
};

export const useGameStore = create<GameState>()(
    persist(
        devtools(
            subscribeWithSelector((set) => ({
                ...initialState, // ← データ部を展開

                /*----- 指し手適用 -----*/
                makeMove: (move) =>
                    set(
                        produce((s: GameState) => {
                            // ① undo で巻き戻した状態から分岐している場合は履歴を切り詰める
                            s.history = s.history.slice(0, s.cursor + 1);
                            s.history.push(move);
                            s.cursor++;

                            // ② 盤面・持ち駒・手番をドメイン関数で更新
                            const { board, hands, nextTurn } = applyMove(
                                s.board,
                                s.hands,
                                s.turn,
                                move,
                            );
                            s.board = board;
                            s.hands = hands;
                            s.turn = nextTurn;

                            // ③ 勝敗判定（TODO: checkmate 判定が実装済みならここで result をセット）
                            // if (isCheckmate(board, nextTurn)) {
                            //   s.result = { winner: toggleSide(nextTurn), reason: "checkmate" };
                            // }
                        }),
                        false,
                        "MAKE_MOVE",
                    ),

                /*----- Undo / Redo -----*/
                undo: () =>
                    set(
                        produce((s) => {
                            if (s.cursor < 0) return;
                            s.cursor--;
                            const snap = replayMoves(
                                { board: initialBoard, hands: createEmptyHands(), turn: "black" },
                                s.history.slice(0, s.cursor + 1),
                            );
                            s.board = snap.board;
                            s.hands = snap.hands;
                            s.turn = snap.turn;
                            s.result = null;
                        }),
                        false,
                        "UNDO",
                    ),

                redo: () =>
                    set(
                        produce((s) => {
                            if (s.cursor >= s.history.length - 1) return;
                            s.cursor++;
                            const snap = replayMoves(
                                { board: initialBoard, hands: createEmptyHands(), turn: "black" },
                                s.history.slice(0, s.cursor + 1),
                            );
                            s.board = snap.board;
                            s.hands = snap.hands;
                            s.turn = snap.turn;
                        }),
                        false,
                        "REDO",
                    ),

                /*----- リセット -----*/
                reset: () => set({ ...initialState }, false, "RESET"),
            })),
        ),
        {
            name: "shogi-game",
            version: 2,
            // 棋譜はストレージに持たせなくても良い場合が多いので除外
            partialize: ({ history, cursor, ...rest }) => rest,
        },
    ),
);
