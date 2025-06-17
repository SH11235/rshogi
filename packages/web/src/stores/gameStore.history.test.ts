import { beforeEach, describe, expect, it } from "vitest";
import { useGameStore } from "./gameStore";

describe("gameStore history navigation", () => {
    beforeEach(() => {
        useGameStore.getState().resetGame();
    });

    describe("basic undo/redo functionality", () => {
        it("should undo and redo a single move", () => {
            // 1手指す
            useGameStore.getState().selectSquare({ row: 7, column: 7 });
            useGameStore.getState().selectSquare({ row: 6, column: 7 });

            expect(useGameStore.getState().moveHistory.length).toBe(1);
            expect(useGameStore.getState().currentPlayer).toBe("white");
            expect(useGameStore.getState().canUndo()).toBe(true);
            expect(useGameStore.getState().canRedo()).toBe(false);

            // 戻る
            useGameStore.getState().undo();

            // undoで開始局面（historyCursor: -2）に戻る
            expect(useGameStore.getState().historyCursor).toBe(-2);
            expect(useGameStore.getState().currentPlayer).toBe("black");
            expect(useGameStore.getState().canRedo()).toBe(true); // 初期位置からredoできる

            // 手動で1手目の状態に移動してテスト
            useGameStore.getState().goToMove(0);

            expect(useGameStore.getState().historyCursor).toBe(0);
            expect(useGameStore.getState().currentPlayer).toBe("white");
            expect(useGameStore.getState().canRedo()).toBe(false); // 最新の手なのでredoできない
        });

        it("should handle multiple moves with undo/redo", () => {
            // 3手指す
            useGameStore.getState().selectSquare({ row: 7, column: 7 });
            useGameStore.getState().selectSquare({ row: 6, column: 7 }); // 先手1手目
            useGameStore.getState().selectSquare({ row: 3, column: 3 });
            useGameStore.getState().selectSquare({ row: 4, column: 3 }); // 後手1手目
            useGameStore.getState().selectSquare({ row: 7, column: 2 });
            useGameStore.getState().selectSquare({ row: 6, column: 2 }); // 先手2手目

            expect(useGameStore.getState().moveHistory.length).toBe(3);
            expect(useGameStore.getState().currentPlayer).toBe("white");

            // 2手戻る
            useGameStore.getState().undo(); // 2手目の状態
            useGameStore.getState().undo(); // 1手目の状態

            expect(useGameStore.getState().historyCursor).toBe(0);
            expect(useGameStore.getState().currentPlayer).toBe("white");
            expect(useGameStore.getState().canUndo()).toBe(true);
            expect(useGameStore.getState().canRedo()).toBe(true);

            // 2手進む
            useGameStore.getState().redo(); // 2手目
            useGameStore.getState().redo(); // 3手目

            expect(useGameStore.getState().historyCursor).toBe(2);
            expect(useGameStore.getState().currentPlayer).toBe("white");
            expect(useGameStore.getState().canRedo()).toBe(false);
        });

        it("should not allow undo when no moves exist", () => {
            expect(useGameStore.getState().canUndo()).toBe(false);
            expect(useGameStore.getState().canRedo()).toBe(false);

            // undoを実行しても何も変わらない
            useGameStore.getState().undo();

            expect(useGameStore.getState().historyCursor).toBe(-1);
            expect(useGameStore.getState().moveHistory.length).toBe(0);
        });

        it("should not allow redo when at latest position", () => {
            // 1手指す
            useGameStore.getState().selectSquare({ row: 7, column: 7 });
            useGameStore.getState().selectSquare({ row: 6, column: 7 });

            expect(useGameStore.getState().canRedo()).toBe(false);

            // redoを実行しても何も変わらない
            const beforeRedo = {
                historyCursor: useGameStore.getState().historyCursor,
                currentPlayer: useGameStore.getState().currentPlayer,
            };

            useGameStore.getState().redo();

            expect(useGameStore.getState().historyCursor).toBe(beforeRedo.historyCursor);
            expect(useGameStore.getState().currentPlayer).toBe(beforeRedo.currentPlayer);
        });
    });

    describe("goToMove functionality", () => {
        it("should jump to specific moves", () => {
            // 3手指す
            useGameStore.getState().selectSquare({ row: 7, column: 7 });
            useGameStore.getState().selectSquare({ row: 6, column: 7 }); // 0
            useGameStore.getState().selectSquare({ row: 3, column: 3 });
            useGameStore.getState().selectSquare({ row: 4, column: 3 }); // 1
            useGameStore.getState().selectSquare({ row: 7, column: 2 });
            useGameStore.getState().selectSquare({ row: 6, column: 2 }); // 2

            // 1手目に移動
            useGameStore.getState().goToMove(0);

            expect(useGameStore.getState().historyCursor).toBe(0);
            expect(useGameStore.getState().currentPlayer).toBe("white");

            // 開始局面に移動
            useGameStore.getState().goToMove(-1);

            expect(useGameStore.getState().historyCursor).toBe(-1);
            expect(useGameStore.getState().currentPlayer).toBe("black");

            // 最終手に移動
            useGameStore.getState().goToMove(2);

            expect(useGameStore.getState().historyCursor).toBe(2);
            expect(useGameStore.getState().currentPlayer).toBe("white");
        });

        it("should handle invalid move indices gracefully", () => {
            // 2手指す
            useGameStore.getState().selectSquare({ row: 7, column: 7 });
            useGameStore.getState().selectSquare({ row: 6, column: 7 });
            useGameStore.getState().selectSquare({ row: 3, column: 3 });
            useGameStore.getState().selectSquare({ row: 4, column: 3 });

            const beforeMove = {
                historyCursor: useGameStore.getState().historyCursor,
                currentPlayer: useGameStore.getState().currentPlayer,
            };

            // 無効なインデックス
            useGameStore.getState().goToMove(10); // 存在しない手
            useGameStore.getState().goToMove(-5); // 無効な負の値

            // 状態が変わらない
            expect(useGameStore.getState().historyCursor).toBe(beforeMove.historyCursor);
            expect(useGameStore.getState().currentPlayer).toBe(beforeMove.currentPlayer);
        });
    });

    describe("board state reconstruction", () => {
        it("should correctly restore board positions", () => {
            // 1手指す
            useGameStore.getState().selectSquare({ row: 7, column: 7 });
            useGameStore.getState().selectSquare({ row: 6, column: 7 });

            // 盤面が変わったことを確認
            expect(useGameStore.getState().board["77"]).toBeNull(); // 元の位置は空
            expect(useGameStore.getState().board["67"]).toBeTruthy(); // 移動先に駒がある

            // 戻る
            useGameStore.getState().undo();

            // 盤面が元に戻ったことを確認
            expect(useGameStore.getState().board["77"]).toBeTruthy(); // 駒が元の位置に戻る
            expect(useGameStore.getState().board["67"]).toBeNull(); // 移動先は空になる

            // 1手目の状態に移動
            useGameStore.getState().goToMove(0);

            // 移動後の盤面に戻る
            expect(useGameStore.getState().board["77"]).toBeNull(); // 元の位置は空
            expect(useGameStore.getState().board["67"]).toBeTruthy(); // 移動先に駒がある
        });
    });

    describe("selection state clearing", () => {
        it("should clear selection states during history navigation", () => {
            // まず1手指してから
            useGameStore.getState().selectSquare({ row: 7, column: 7 });
            useGameStore.getState().selectSquare({ row: 6, column: 7 });

            // 後手番なので後手の駒を選択
            useGameStore.getState().selectSquare({ row: 3, column: 3 });

            expect(useGameStore.getState().selectedSquare).toBeTruthy();
            expect(useGameStore.getState().validMoves.length).toBeGreaterThan(0);

            // 履歴操作を実行
            useGameStore.getState().undo();

            // 選択状態がクリアされる
            expect(useGameStore.getState().selectedSquare).toBeNull();
            expect(useGameStore.getState().validMoves).toEqual([]);
            expect(useGameStore.getState().selectedDropPiece).toBeNull();
            expect(useGameStore.getState().validDropSquares).toEqual([]);
        });
    });

    describe("history truncation", () => {
        it("should truncate future history when making new moves from past", () => {
            // 3手指す
            useGameStore.getState().selectSquare({ row: 7, column: 7 });
            useGameStore.getState().selectSquare({ row: 6, column: 7 }); // 0
            useGameStore.getState().selectSquare({ row: 3, column: 3 });
            useGameStore.getState().selectSquare({ row: 4, column: 3 }); // 1
            useGameStore.getState().selectSquare({ row: 7, column: 2 });
            useGameStore.getState().selectSquare({ row: 6, column: 2 }); // 2

            // 1手戻る
            useGameStore.getState().undo();
            expect(useGameStore.getState().historyCursor).toBe(1);

            // 新しい手を指す
            useGameStore.getState().selectSquare({ row: 7, column: 6 });
            useGameStore.getState().selectSquare({ row: 6, column: 6 });

            // 履歴が正しく切り詰められる
            expect(useGameStore.getState().moveHistory.length).toBe(3);
            expect(useGameStore.getState().historyCursor).toBe(-1); // 最新状態

            // 最後の手が新しい手になっている
            const lastMove = useGameStore.getState().moveHistory[2];
            if (lastMove.type === "move") {
                expect(lastMove.from).toEqual({ row: 7, column: 6 });
                expect(lastMove.to).toEqual({ row: 6, column: 6 });
            }
        });
    });

    describe("goToMove interaction with undo/redo", () => {
        it("should work correctly after using goToMove to jump to a specific move", () => {
            // 3手指す
            useGameStore.getState().selectSquare({ row: 7, column: 7 });
            useGameStore.getState().selectSquare({ row: 6, column: 7 }); // 先手1手目
            useGameStore.getState().selectSquare({ row: 3, column: 3 });
            useGameStore.getState().selectSquare({ row: 4, column: 3 }); // 後手1手目
            useGameStore.getState().selectSquare({ row: 7, column: 2 });
            useGameStore.getState().selectSquare({ row: 6, column: 2 }); // 先手2手目

            expect(useGameStore.getState().moveHistory.length).toBe(3);
            expect(useGameStore.getState().historyCursor).toBe(-1);

            // 1手目にジャンプ
            useGameStore.getState().goToMove(0);
            expect(useGameStore.getState().historyCursor).toBe(0);

            // undo: 0 → -2 (初期位置)
            useGameStore.getState().undo();
            expect(useGameStore.getState().historyCursor).toBe(-2);
            expect(useGameStore.getState().canRedo()).toBe(true);

            // redo: -2 → 0 (1手目)
            useGameStore.getState().redo();
            expect(useGameStore.getState().historyCursor).toBe(0);
            expect(useGameStore.getState().canRedo()).toBe(true);

            // redo: 0 → 1 (2手目)
            useGameStore.getState().redo();
            expect(useGameStore.getState().historyCursor).toBe(1);
            expect(useGameStore.getState().canRedo()).toBe(true);
        });

        it("should work correctly when clicking initial position button", () => {
            // 2手指す
            useGameStore.getState().selectSquare({ row: 7, column: 7 });
            useGameStore.getState().selectSquare({ row: 6, column: 7 }); // 先手1手目
            useGameStore.getState().selectSquare({ row: 3, column: 3 });
            useGameStore.getState().selectSquare({ row: 4, column: 3 }); // 後手1手目

            expect(useGameStore.getState().moveHistory.length).toBe(2);
            expect(useGameStore.getState().historyCursor).toBe(-1);

            // 初期位置にジャンプ（開始局面ボタン）
            useGameStore.getState().goToMove(-2);
            expect(useGameStore.getState().historyCursor).toBe(-2);
            expect(useGameStore.getState().canUndo()).toBe(false);
            expect(useGameStore.getState().canRedo()).toBe(true);

            // redo: -2 → 0 (1手目)
            useGameStore.getState().redo();
            expect(useGameStore.getState().historyCursor).toBe(0);

            // undo: 0 → -2 (初期位置)
            useGameStore.getState().undo();
            expect(useGameStore.getState().historyCursor).toBe(-2);
        });
    });
});
