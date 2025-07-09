import type { Column, Move, Row } from "shogi-core";
import { beforeEach, describe, expect, it } from "vitest";
import { useGameStore } from "./gameStore";

describe("gameStore getCurrentSfen", () => {
    beforeEach(() => {
        useGameStore.setState(useGameStore.getInitialState());
    });

    it("初期盤面のSFENを返す", () => {
        const store = useGameStore.getState();
        const sfen = store.getCurrentSfen();

        expect(sfen).toBe("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1");
    });

    it("手を指した後の正しいSFENを返す", () => {
        const store = useGameStore.getState();

        // selectSquareを使って手を指す
        store.selectSquare({ row: 7 as Row, column: 7 as Column }); // 7七の歩を選択
        store.selectSquare({ row: 6 as Row, column: 7 as Column }); // 7六に移動

        const sfen = store.getCurrentSfen();

        // 7七の歩を7六に動かした後のSFEN（moveNumber=2）
        expect(sfen).toBe("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2");
    });

    it("履歴移動中の正しいSFENを返す", () => {
        const store = useGameStore.getState();

        // いくつか手を指す
        store.selectSquare({ row: 7 as Row, column: 7 as Column });
        store.selectSquare({ row: 6 as Row, column: 7 as Column }); // 7七歩→7六

        store.selectSquare({ row: 3 as Row, column: 3 as Column });
        store.selectSquare({ row: 4 as Row, column: 3 as Column }); // 3三歩→3四

        store.selectSquare({ row: 7 as Row, column: 2 as Column });
        store.selectSquare({ row: 6 as Row, column: 2 as Column }); // 2七歩→2六

        // 1手目まで戻る（0が1手目の後）
        store.goToMove(0);
        const sfen = store.getCurrentSfen();

        // 1手目の後のSFEN（手番は白、moveNumber=2）
        expect(sfen).toBe("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2");
    });

    it("分岐中の正しいSFENを返す", () => {
        const store = useGameStore.getState();

        // いくつか手を指す
        store.selectSquare({ row: 7 as Row, column: 7 as Column });
        store.selectSquare({ row: 6 as Row, column: 7 as Column }); // 7七歩→7六

        store.selectSquare({ row: 3 as Row, column: 3 as Column });
        store.selectSquare({ row: 4 as Row, column: 3 as Column }); // 3三歩→3四

        // 1手目まで戻る（0が最初の手の後）
        store.goToMove(0);

        // 別の手を指して分岐を作る（後手番なので後手の駒を動かす）
        store.selectSquare({ row: 3 as Row, column: 1 as Column });
        store.selectSquare({ row: 4 as Row, column: 1 as Column }); // 1三歩→1四（分岐）

        const sfen = store.getCurrentSfen();

        // デバッグ情報を出力
        const state = store;
        console.log("historyCursor:", state.historyCursor);
        console.log("moveHistory length:", state.moveHistory.length);
        console.log("branchInfo:", state.branchInfo);
        console.log("gameMode:", state.gameMode);

        // 分岐した手の後のSFEN（後手が1三歩を1四に動かした後）
        // 現在の手番は先手（b）、手数は2（分岐なので元の履歴位置+1）
        expect(sfen).toBe(
            "sfen lnsgkgsnl/1r5b1/1pppppppp/p8/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 2",
        );
    });

    it("閲覧モードでの正しいSFENを返す", () => {
        // 棋譜を読み込む（簡単な例）
        const moves: Move[] = [
            {
                type: "move",
                from: { row: 7 as Row, column: 7 as Column },
                to: { row: 6 as Row, column: 7 as Column },
                piece: { type: "pawn", owner: "black", promoted: false },
                promote: false,
                captured: null,
            },
        ];

        // importGame の第2引数に棋譜内容を渡す
        useGameStore.getState().importGame(moves, "▲７六歩");

        // 状態を再取得
        const store = useGameStore.getState();

        // 閲覧モードに入っている
        expect(store.gameMode).toBe("review");

        // 最後の手まで進める（importGameは初期位置から始まるため）
        store.goToMove(0);

        const sfen = store.getCurrentSfen();

        // 1手目の後のSFEN（moveNumber=2）
        expect(sfen).toBe("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2");
    });
});
