import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
    applyMove,
    applyMoveWithState,
    boardFromMoves,
    canPass,
    cloneBoard,
    createEmptyHands,
    createInitialBoard,
    createInitialPositionState,
    getAllSquares,
    type Hands,
    isPassMove,
    type PositionState,
    parseMove,
    replayMoves,
} from "./board";
import type { PositionService } from "./position-service";
import { setPositionServiceFactory } from "./position-service-registry";

// モックの PositionService を作成
const createMockPositionService = (): PositionService => {
    const initialPosition: PositionState = {
        board: createInitialBoard(),
        hands: createEmptyHands(),
        turn: "sente",
        ply: 1,
    };

    return {
        async getInitialBoard() {
            return initialPosition;
        },
        async parseSfen(_sfen: string) {
            return initialPosition;
        },
        async boardToSfen(_position: PositionState) {
            return "startpos";
        },
        async getLegalMoves(_sfen: string, _moves?: string[]) {
            return [];
        },
        async replayMovesStrict(
            _sfen: string,
            moves: string[],
            _options?: { passRights?: { sente: number; gote: number } },
        ) {
            return {
                applied: moves,
                lastPly: moves.length,
                position: initialPosition,
            };
        },
    };
};

beforeEach(() => {
    setPositionServiceFactory(() => createMockPositionService());
});

afterEach(() => {
    vi.clearAllMocks();
});

describe("parseMove", () => {
    it("通常の移動をパースする (7g7f)", () => {
        const result = parseMove("7g7f");
        expect(result).toEqual({
            kind: "move",
            from: "7g",
            to: "7f",
            promote: false,
        });
    });

    it("成りの移動をパースする (7g7f+)", () => {
        const result = parseMove("7g7f+");
        expect(result).toEqual({
            kind: "move",
            from: "7g",
            to: "7f",
            promote: true,
        });
    });

    it("駒を打つ手をパースする (P*5e)", () => {
        const result = parseMove("P*5e");
        expect(result).toEqual({
            kind: "drop",
            to: "5e",
            piece: "P",
        });
    });

    it("小文字の駒も正しくパースする (p*5e)", () => {
        const result = parseMove("p*5e");
        expect(result).toEqual({
            kind: "drop",
            to: "5e",
            piece: "P",
        });
    });

    it("王を打つ手は null を返す", () => {
        const result = parseMove("K*5e");
        expect(result).toBeNull();
    });

    it("不正なフォーマットは null を返す", () => {
        expect(parseMove("")).toBeNull();
        expect(parseMove("abc")).toBeNull();
        expect(parseMove("7g")).toBeNull();
        expect(parseMove("7g7")).toBeNull();
    });

    it("盤外のマスは null を返す", () => {
        expect(parseMove("0a1a")).toBeNull();
        expect(parseMove("9j9i")).toBeNull();
    });
});

describe("applyMoveWithState", () => {
    describe("基本的な駒の移動", () => {
        it("通常の駒の移動を正しく適用する", () => {
            const initialState = createInitialPositionState();
            const result = applyMoveWithState(initialState, "7g7f", { validateTurn: false });

            expect(result.ok).toBe(true);
            expect(result.next.board["7g"]).toBeNull();
            expect(result.next.board["7f"]).toEqual({
                owner: "sente",
                type: "P",
            });
            expect(result.next.turn).toBe("gote");
            expect(result.lastMove).toEqual({
                from: "7g",
                to: "7f",
                promotes: false,
            });
        });

        it("駒を取る手を正しく処理する", () => {
            const board = createInitialBoard();
            // 先手の銀を3dに配置して、後手の歩（3c）を取る
            board["3d"] = { owner: "sente", type: "S" };

            const state: PositionState = {
                board,
                hands: createEmptyHands(),
                turn: "sente",
            };

            const result = applyMoveWithState(state, "3d3c", { validateTurn: false });

            expect(result.ok).toBe(true);
            // 取った歩が持ち駒に追加される
            expect(result.next.hands.sente.P).toBe(1);
        });

        it("自駒への移動はエラーを返す", () => {
            const initialState = createInitialPositionState();
            // 7gから7iへ移動（7iには香車がいる）
            const result = applyMoveWithState(initialState, "7g7i", { validateTurn: false });

            expect(result.ok).toBe(false);
            expect(result.error).toBe("cannot capture own piece");
        });

        it("駒がないマスからの移動はエラーを返す", () => {
            const initialState = createInitialPositionState();
            const result = applyMoveWithState(initialState, "5e5f", { validateTurn: false });

            expect(result.ok).toBe(false);
            expect(result.error).toBe("no piece at source square");
        });

        it("相手の駒を動かそうとするとエラーを返す (validateTurn: true)", () => {
            const initialState = createInitialPositionState();
            // 先手番で後手の駒を動かそうとする
            const result = applyMoveWithState(initialState, "3c3d", { validateTurn: true });

            expect(result.ok).toBe(false);
            expect(result.error).toBe("not your turn");
        });
    });

    describe("成りの処理", () => {
        it("成りフラグ付きの手を正しく適用する", () => {
            const board = createInitialBoard();
            const hands = createEmptyHands();

            // 先手の歩を1cに配置（成れる位置）
            board["1c"] = { owner: "sente", type: "P" };

            const state: PositionState = { board, hands, turn: "sente" };
            const result = applyMoveWithState(state, "1c1b+", { validateTurn: false });

            expect(result.ok).toBe(true);
            expect(result.next.board["1b"]).toEqual({
                owner: "sente",
                type: "P",
                promoted: true,
            });
            expect(result.lastMove?.promotes).toBe(true);
        });

        it("既に成っている駒の promoted フラグが保持される", () => {
            const board = createInitialBoard();
            const hands = createEmptyHands();

            // 成った歩を配置
            board["5e"] = { owner: "sente", type: "P", promoted: true };

            const state: PositionState = { board, hands, turn: "sente" };
            const result = applyMoveWithState(state, "5e5d", { validateTurn: false });

            expect(result.ok).toBe(true);
            expect(result.next.board["5d"]?.promoted).toBe(true);
        });
    });

    describe("持ち駒の管理", () => {
        it("駒を打つ手を正しく適用する", () => {
            const board = createInitialBoard();
            const hands: Hands = {
                sente: { P: 2 },
                gote: {},
            };

            const state: PositionState = { board, hands, turn: "sente" };
            const result = applyMoveWithState(state, "P*5e", { validateTurn: false });

            expect(result.ok).toBe(true);
            expect(result.next.board["5e"]).toEqual({
                owner: "sente",
                type: "P",
            });
            expect(result.next.hands.sente.P).toBe(1);
            expect(result.lastMove).toEqual({
                from: null,
                to: "5e",
                dropPiece: "P",
                promotes: false,
            });
        });

        it("持ち駒がない場合はエラーを返す", () => {
            const board = createInitialBoard();
            const hands = createEmptyHands();

            const state: PositionState = { board, hands, turn: "sente" };
            const result = applyMoveWithState(state, "P*5e", { validateTurn: false });

            expect(result.ok).toBe(false);
            expect(result.error).toBe("no piece in hand");
        });

        it("駒がいるマスに打てない", () => {
            const board = createInitialBoard();
            const hands: Hands = {
                sente: { P: 1 },
                gote: {},
            };

            const state: PositionState = { board, hands, turn: "sente" };
            const result = applyMoveWithState(state, "P*7g", { validateTurn: false });

            expect(result.ok).toBe(false);
            expect(result.error).toBe("cannot drop onto occupied square");
        });

        it("成り駒を取ると元の駒種で持ち駒に追加される", () => {
            const board = createInitialBoard();
            const hands = createEmptyHands();

            // 後手の成り歩を配置
            board["3c"] = { owner: "gote", type: "P", promoted: true };
            // 先手の駒を隣に配置
            board["3d"] = { owner: "sente", type: "S" };

            const state: PositionState = { board, hands, turn: "sente" };
            const result = applyMoveWithState(state, "3d3c", { validateTurn: false });

            expect(result.ok).toBe(true);
            // 成り駒を取ると元の駒種（P）で持ち駒に追加される
            expect(result.next.hands.sente.P).toBe(1);
        });

        it("ignoreHandLimits オプションで持ち駒チェックをスキップできる", () => {
            const board = createInitialBoard();
            const hands = createEmptyHands();

            const state: PositionState = { board, hands, turn: "sente" };
            const result = applyMoveWithState(state, "P*5e", {
                validateTurn: false,
                ignoreHandLimits: true,
            });

            expect(result.ok).toBe(true);
            expect(result.next.board["5e"]).toEqual({
                owner: "sente",
                type: "P",
            });
        });
    });

    describe("エラーハンドリング", () => {
        it("空文字列の手はエラーを返す", () => {
            const initialState = createInitialPositionState();
            const result = applyMoveWithState(initialState, "", { validateTurn: false });

            expect(result.ok).toBe(false);
            expect(result.error).toBe("move is empty or resign");
        });

        it("resign トークンはエラーを返す", () => {
            const initialState = createInitialPositionState();
            const result = applyMoveWithState(initialState, "resign", { validateTurn: false });

            expect(result.ok).toBe(false);
            expect(result.error).toBe("move is empty or resign");
        });

        it("不正なフォーマットはエラーを返す", () => {
            const initialState = createInitialPositionState();
            const result = applyMoveWithState(initialState, "invalid", { validateTurn: false });

            expect(result.ok).toBe(false);
            expect(result.error).toBe("invalid move format");
        });
    });
});

describe("replayMoves", () => {
    it("複数の手を順番に適用する", () => {
        const moves = ["7g7f", "3c3d"];
        const result = replayMoves(moves, { validateTurn: false });

        expect(result.errors).toHaveLength(0);
        expect(result.state.board["7f"]).toEqual({
            owner: "sente",
            type: "P",
        });
        expect(result.state.board["3d"]).toEqual({
            owner: "gote",
            type: "P",
        });
    });

    it("エラーが発生した場合、errors 配列に追加する", () => {
        const moves = ["7g7f", "invalid", "3c3d"];
        const result = replayMoves(moves, { validateTurn: false });

        expect(result.errors).toHaveLength(1);
        expect(result.errors[0]).toContain("move 2");
        expect(result.errors[0]).toContain("invalid move format");
    });

    it("初期局面から正しく再生する", () => {
        const moves = ["7g7f"];
        const result = replayMoves(moves, { validateTurn: false });

        expect(result.state.board["7g"]).toBeNull();
        expect(result.state.board["7f"]).toEqual({
            owner: "sente",
            type: "P",
        });
    });

    it("lastMove が正しく設定される", () => {
        const moves = ["7g7f", "3c3d"];
        const result = replayMoves(moves, { validateTurn: false });

        expect(result.lastMove).toEqual({
            from: "3c",
            to: "3d",
            promotes: false,
        });
    });
});

describe("boardFromMoves", () => {
    it("手順から盤面を構築する", () => {
        const moves = ["7g7f", "3c3d"];
        const board = boardFromMoves(moves);

        expect(board["7f"]).toEqual({
            owner: "sente",
            type: "P",
        });
        expect(board["3d"]).toEqual({
            owner: "gote",
            type: "P",
        });
    });
});

describe("cloneBoard", () => {
    it("盤面を正しく複製する", () => {
        const original = createInitialBoard();
        const clone = cloneBoard(original);

        // 全てのマスを確認
        const squares = getAllSquares();
        squares.forEach((square) => {
            const origPiece = original[square];
            const clonePiece = clone[square];

            if (origPiece === null) {
                expect(clonePiece).toBeNull();
            } else {
                expect(clonePiece).toEqual(origPiece);
                // 異なるオブジェクトであることを確認
                expect(clonePiece).not.toBe(origPiece);
            }
        });
    });

    it("複製した盤面を変更しても元の盤面に影響しない", () => {
        const original = createInitialBoard();
        const clone = cloneBoard(original);

        // クローンを変更
        clone["5e"] = { owner: "sente", type: "K" };

        // 元の盤面は変更されていない
        expect(original["5e"]).toBeNull();
        expect(clone["5e"]).toEqual({
            owner: "sente",
            type: "K",
        });
    });
});

describe("applyMove", () => {
    it("手を適用して新しい盤面を返す", () => {
        const board = createInitialBoard();
        const newBoard = applyMove(board, "7g7f");

        expect(board["7g"]).toEqual({
            owner: "sente",
            type: "P",
        });
        expect(newBoard["7g"]).toBeNull();
        expect(newBoard["7f"]).toEqual({
            owner: "sente",
            type: "P",
        });
    });
});

describe("createEmptyHands", () => {
    it("空の持ち駒を作成する", () => {
        const hands = createEmptyHands();

        expect(hands.sente).toEqual({});
        expect(hands.gote).toEqual({});
    });
});

describe("getAllSquares", () => {
    it("全てのマス目を返す（81マス）", () => {
        const squares = getAllSquares();

        expect(squares).toHaveLength(81);
        expect(squares).toContain("9a");
        expect(squares).toContain("1i");
        expect(squares).toContain("5e");
    });
});

describe("パス手の処理", () => {
    describe("parseMove - パス手のパース", () => {
        it("'pass' をパスとしてパースする", () => {
            const result = parseMove("pass");
            expect(result).toEqual({ kind: "pass" });
        });

        it("'PASS' (大文字) もパスとしてパースする", () => {
            const result = parseMove("PASS");
            expect(result).toEqual({ kind: "pass" });
        });

        it("'Pass' (混在) もパスとしてパースする", () => {
            const result = parseMove("Pass");
            expect(result).toEqual({ kind: "pass" });
        });
    });

    describe("isPassMove", () => {
        it("'pass' は true を返す", () => {
            expect(isPassMove("pass")).toBe(true);
        });

        it("'PASS' (大文字) も true を返す", () => {
            expect(isPassMove("PASS")).toBe(true);
        });

        it("通常の指し手は false を返す", () => {
            expect(isPassMove("7g7f")).toBe(false);
            expect(isPassMove("P*5e")).toBe(false);
        });
    });

    describe("canPass", () => {
        it("パス権が残っている場合は true を返す", () => {
            const state: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "sente",
                passRights: { sente: 2, gote: 2 },
            };
            expect(canPass(state)).toBe(true);
        });

        it("パス権が0の場合は false を返す", () => {
            const state: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "sente",
                passRights: { sente: 0, gote: 2 },
            };
            expect(canPass(state)).toBe(false);
        });

        it("パス権が有効でない場合は false を返す", () => {
            const state: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "sente",
            };
            expect(canPass(state)).toBe(false);
        });

        it("後手番で後手のパス権が残っている場合は true を返す", () => {
            const state: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "gote",
                passRights: { sente: 0, gote: 1 },
            };
            expect(canPass(state)).toBe(true);
        });
    });

    describe("applyMoveWithState - パス手の適用", () => {
        it("パス権が有効な場合にパスが成功する", () => {
            const state: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "sente",
                passRights: { sente: 2, gote: 2 },
            };

            const result = applyMoveWithState(state, "pass", { validateTurn: false });

            expect(result.ok).toBe(true);
            expect(result.next.passRights?.sente).toBe(1);
            expect(result.next.passRights?.gote).toBe(2);
            expect(result.next.turn).toBe("gote");
            expect(result.lastMove?.isPass).toBe(true);
        });

        it("パス権が0の場合はエラーを返す", () => {
            const state: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "sente",
                passRights: { sente: 0, gote: 2 },
            };

            const result = applyMoveWithState(state, "pass", { validateTurn: false });

            expect(result.ok).toBe(false);
            expect(result.error).toBe("no pass rights remaining");
        });

        it("パス権が有効でない場合はエラーを返す", () => {
            const state: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "sente",
            };

            const result = applyMoveWithState(state, "pass", { validateTurn: false });

            expect(result.ok).toBe(false);
            expect(result.error).toBe("pass rights not enabled");
        });

        it("後手がパスすると後手のパス権が減る", () => {
            const state: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "gote",
                passRights: { sente: 2, gote: 2 },
            };

            const result = applyMoveWithState(state, "pass", { validateTurn: false });

            expect(result.ok).toBe(true);
            expect(result.next.passRights?.sente).toBe(2);
            expect(result.next.passRights?.gote).toBe(1);
            expect(result.next.turn).toBe("sente");
        });

        it("パス手の後も盤面は変わらない", () => {
            const state: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "sente",
                passRights: { sente: 2, gote: 2 },
            };

            const result = applyMoveWithState(state, "pass", { validateTurn: false });

            expect(result.ok).toBe(true);
            // 盤面が変わっていないことを確認（7g の歩がそのまま）
            expect(result.next.board["7g"]).toEqual({ owner: "sente", type: "P" });
        });

        it("連続してパスできる", () => {
            let state: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "sente",
                passRights: { sente: 2, gote: 2 },
            };

            // 先手パス
            const result1 = applyMoveWithState(state, "pass", { validateTurn: false });
            expect(result1.ok).toBe(true);
            state = result1.next;

            // 後手パス
            const result2 = applyMoveWithState(state, "pass", { validateTurn: false });
            expect(result2.ok).toBe(true);
            state = result2.next;

            // 先手パス（2回目）
            const result3 = applyMoveWithState(state, "pass", { validateTurn: false });
            expect(result3.ok).toBe(true);

            expect(result3.next.passRights?.sente).toBe(0);
            expect(result3.next.passRights?.gote).toBe(1);
        });
    });

    describe("replayMoves - パス手を含む棋譜の再生", () => {
        it("パス手を含む棋譜を正しく再生する", () => {
            const initialState: PositionState = {
                board: createInitialBoard(),
                hands: createEmptyHands(),
                turn: "sente",
                passRights: { sente: 2, gote: 2 },
            };

            const moves = ["7g7f", "pass", "2g2f"];
            const result = replayMoves(moves, { validateTurn: false }, initialState);

            expect(result.errors).toHaveLength(0);
            expect(result.state.passRights?.gote).toBe(1); // 後手がパスした
            expect(result.state.board["7f"]).toEqual({ owner: "sente", type: "P" });
            expect(result.state.board["2f"]).toEqual({ owner: "sente", type: "P" });
        });
    });
});
