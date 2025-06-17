import { HISTORY_CURSOR } from "@/constants/history";
import { beforeEach, describe, expect, it } from "vitest";
import { useGameStore } from "./gameStore";

describe("gameStore move history", () => {
    beforeEach(() => {
        useGameStore.getState().resetGame();
    });

    it("should correctly track move numbers", () => {
        // 先手1手目：7七歩
        useGameStore.getState().selectSquare({ row: 7, column: 7 });
        useGameStore.getState().selectSquare({ row: 6, column: 7 });

        expect(useGameStore.getState().moveHistory.length).toBe(1);

        // 後手1手目：3三歩
        useGameStore.getState().selectSquare({ row: 3, column: 3 });
        useGameStore.getState().selectSquare({ row: 4, column: 3 });

        expect(useGameStore.getState().moveHistory.length).toBe(2);

        // 先手2手目：2六歩
        useGameStore.getState().selectSquare({ row: 7, column: 2 });
        useGameStore.getState().selectSquare({ row: 6, column: 2 });

        expect(useGameStore.getState().moveHistory.length).toBe(3);
    });

    it("should not increase total moves when using undo/redo", () => {
        // 2手指す
        useGameStore.getState().selectSquare({ row: 7, column: 7 });
        useGameStore.getState().selectSquare({ row: 6, column: 7 });
        useGameStore.getState().selectSquare({ row: 3, column: 3 });
        useGameStore.getState().selectSquare({ row: 4, column: 3 });

        expect(useGameStore.getState().moveHistory.length).toBe(2);

        // 戻る
        useGameStore.getState().undo();
        expect(useGameStore.getState().moveHistory.length).toBe(2); // 履歴は削除されない
        expect(useGameStore.getState().historyCursor).toBe(0); // カーソルが変わる

        // 進む
        useGameStore.getState().redo();
        expect(useGameStore.getState().moveHistory.length).toBe(2); // 履歴は変わらない
        expect(useGameStore.getState().historyCursor).toBe(1);

        // もう一度戻る
        useGameStore.getState().undo();
        expect(useGameStore.getState().moveHistory.length).toBe(2);
        expect(useGameStore.getState().historyCursor).toBe(0);
    });

    it("should truncate future history when making a new move from the past", () => {
        // 3手指す
        useGameStore.getState().selectSquare({ row: 7, column: 7 });
        useGameStore.getState().selectSquare({ row: 6, column: 7 });
        useGameStore.getState().selectSquare({ row: 3, column: 3 });
        useGameStore.getState().selectSquare({ row: 4, column: 3 });
        useGameStore.getState().selectSquare({ row: 7, column: 2 });
        useGameStore.getState().selectSquare({ row: 6, column: 2 });

        expect(useGameStore.getState().moveHistory.length).toBe(3);

        // 1手戻る
        useGameStore.getState().undo();
        expect(useGameStore.getState().moveHistory.length).toBe(3);
        expect(useGameStore.getState().historyCursor).toBe(1);

        // 新しい手を指す（3手目とは異なる手）
        useGameStore.getState().selectSquare({ row: 7, column: 6 });
        useGameStore.getState().selectSquare({ row: 6, column: 6 });

        // 未来の履歴（元の3手目）が削除され、新しい手が追加される
        expect(useGameStore.getState().moveHistory.length).toBe(3);
        expect(useGameStore.getState().historyCursor).toBe(HISTORY_CURSOR.LATEST_POSITION); // 最新状態

        // 最後の手が新しい手になっている
        const lastMove = useGameStore.getState().moveHistory[2];
        expect(lastMove.from).toEqual({ row: 7, column: 6 });
        expect(lastMove.to).toEqual({ row: 6, column: 6 });
    });
});
