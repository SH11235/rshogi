import type { BoardState, Hands, PositionState, Square } from "@shogi/app-core";
import { describe, expect, it } from "vitest";
import { applyDrop, applyDropResult, validateDrop } from "./dropLogic";
import type { DragOrigin, DragPayload, DropResult, DropTarget } from "./types";

// テスト用ヘルパー
const createEmptyBoard = (): BoardState => {
    const squares: Square[] = [];
    const files = ["9", "8", "7", "6", "5", "4", "3", "2", "1"];
    const ranks = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    for (const rank of ranks) {
        for (const file of files) {
            squares.push(`${file}${rank}` as Square);
        }
    }
    return Object.fromEntries(squares.map((sq) => [sq, null])) as BoardState;
};

const createEmptyHands = (): Hands => ({ sente: {}, gote: {} });

const createEmptyPosition = (): PositionState => ({
    board: createEmptyBoard(),
    hands: createEmptyHands(),
    turn: "sente",
    ply: 1,
});

describe("dropLogic", () => {
    describe("validateDrop", () => {
        it("削除は許可される（通常の駒）", () => {
            const position = createEmptyPosition();
            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "delete" };

            const result = validateDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
        });

        it("玉は削除できない", () => {
            const position = createEmptyPosition();
            position.board["5i"] = { owner: "sente", type: "K" };

            const origin: DragOrigin = { type: "board", square: "5i" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "K",
                isPromoted: false,
            };
            const target: DropTarget = { type: "delete" };

            const result = validateDrop(origin, payload, target, position);

            expect(result.ok).toBe(false);
            expect(result.error).toBe("玉は削除できません");
        });

        it("盤上から盤上への移動を許可する", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "P" };

            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "board", square: "5d" };

            const result = validateDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
        });

        it("自分の駒がある場所への配置を拒否する", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "P" };
            position.board["5d"] = { owner: "sente", type: "G" };

            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "board", square: "5d" };

            const result = validateDrop(origin, payload, target, position);

            expect(result.ok).toBe(false);
            expect(result.error).toBe("自分の駒がある場所には置けません");
        });

        it("同じ場所への移動は許可する（キャンセル扱い）", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "P" };

            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "board", square: "5e" };

            const result = validateDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
        });

        it("敵の駒がある場所への移動を許可する", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "P" };
            position.board["5d"] = { owner: "gote", type: "P" };

            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "board", square: "5d" };

            const result = validateDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
        });

        it("持ち駒から盤上への配置を許可する", () => {
            const position = createEmptyPosition();
            position.hands.sente.P = 1;

            const origin: DragOrigin = { type: "hand", owner: "sente", pieceType: "P" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "board", square: "5e" };

            const result = validateDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
        });

        it("盤上から持ち駒エリアへの移動を許可する", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "P" };

            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "hand", owner: "sente" };

            const result = validateDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
        });

        it("持ち駒から持ち駒への移動を拒否する", () => {
            const position = createEmptyPosition();
            position.hands.sente.P = 1;

            const origin: DragOrigin = { type: "hand", owner: "sente", pieceType: "P" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "hand", owner: "gote" };

            const result = validateDrop(origin, payload, target, position);

            expect(result.ok).toBe(false);
            expect(result.error).toBe("持ち駒から持ち駒への移動はできません");
        });

        it("玉は1枚までの制限を検証する", () => {
            const position = createEmptyPosition();
            position.board["5i"] = { owner: "sente", type: "K" };

            const origin: DragOrigin = { type: "stock", owner: "sente", pieceType: "K" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "K",
                isPromoted: false,
            };
            const target: DropTarget = { type: "board", square: "5h" };

            const result = validateDrop(origin, payload, target, position);

            expect(result.ok).toBe(false);
            // 駒数制限チェックが先に行われるため、PIECE_CAP のエラーメッセージが返される
            expect(result.error).toBe("Kは最大1枚までです");
        });
    });

    describe("applyDrop", () => {
        it("盤上から盤上への移動を適用する", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "P" };

            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "board", square: "5d" };

            const result = applyDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
            expect(result.next.board["5e"]).toBeNull();
            expect(result.next.board["5d"]).toEqual({ owner: "sente", type: "P" });
        });

        it("敵の駒を取って持ち駒に追加する", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "R" };
            position.board["5d"] = { owner: "gote", type: "P" };

            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "R",
                isPromoted: false,
            };
            const target: DropTarget = { type: "board", square: "5d" };

            const result = applyDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
            expect(result.next.board["5e"]).toBeNull();
            expect(result.next.board["5d"]).toEqual({ owner: "sente", type: "R" });
            expect(result.next.hands.sente.P).toBe(1);
        });

        it("削除を適用する", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "P" };

            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "delete" };

            const result = applyDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
            expect(result.next.board["5e"]).toBeNull();
        });

        it("持ち駒エリアへの移動を適用する", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "P", promoted: true };

            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: true,
            };
            const target: DropTarget = { type: "hand", owner: "sente" };

            const result = applyDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
            expect(result.next.board["5e"]).toBeNull();
            expect(result.next.hands.sente.P).toBe(1);
        });

        it("持ち駒から盤上への配置を適用する", () => {
            const position = createEmptyPosition();
            position.hands.sente.P = 2;

            const origin: DragOrigin = { type: "hand", owner: "sente", pieceType: "P" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "board", square: "5e" };

            const result = applyDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
            expect(result.next.board["5e"]).toEqual({ owner: "sente", type: "P" });
            expect(result.next.hands.sente.P).toBe(1);
        });

        it("同じ場所への移動は元に戻す", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "P" };

            const origin: DragOrigin = { type: "board", square: "5e" };
            const payload: DragPayload = {
                owner: "sente",
                pieceType: "P",
                isPromoted: false,
            };
            const target: DropTarget = { type: "board", square: "5e" };

            const result = applyDrop(origin, payload, target, position);

            expect(result.ok).toBe(true);
            expect(result.next.board["5e"]).toEqual({ owner: "sente", type: "P" });
        });
    });

    describe("applyDropResult", () => {
        it("DropResult からドロップを適用する", () => {
            const position = createEmptyPosition();
            position.board["5e"] = { owner: "sente", type: "P" };

            const dropResult: DropResult = {
                origin: { type: "board", square: "5e" },
                payload: { owner: "sente", pieceType: "P", isPromoted: false },
                target: { type: "board", square: "5d" },
            };

            const result = applyDropResult(dropResult, position);

            expect(result.ok).toBe(true);
            expect(result.next.board["5e"]).toBeNull();
            expect(result.next.board["5d"]).toEqual({ owner: "sente", type: "P" });
        });
    });
});
