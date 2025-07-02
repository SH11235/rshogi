import { describe, expect, it } from "vitest";
import type { Board } from "../model/board";
import type { Player } from "../model/piece";
import type { SquareKey } from "../model/square";
import { initialHands } from "./moveService";
import {
    type PositionState,
    checkJishogi,
    checkMaxMoves,
    checkPerpetualCheck,
    checkSennichite,
    checkTryRule,
    hashPosition,
} from "./repetitionService";

describe("repetitionService", () => {
    describe("hashPosition", () => {
        it("同じ局面は同じハッシュ値を返す", () => {
            const position1: PositionState = {
                board: {
                    "11": { type: "king", owner: "black", promoted: false },
                    "19": { type: "king", owner: "white", promoted: false },
                    "23": { type: "pawn", owner: "black", promoted: false },
                } as Board,
                hands: initialHands(),
                currentPlayer: "black",
            };

            const position2: PositionState = {
                board: {
                    "11": { type: "king", owner: "black", promoted: false },
                    "19": { type: "king", owner: "white", promoted: false },
                    "23": { type: "pawn", owner: "black", promoted: false },
                } as Board,
                hands: initialHands(),
                currentPlayer: "black",
            };

            expect(hashPosition(position1)).toBe(hashPosition(position2));
        });

        it("異なる手番は異なるハッシュ値を返す", () => {
            const position1: PositionState = {
                board: {
                    "11": { type: "king", owner: "black", promoted: false },
                } as Board,
                hands: initialHands(),
                currentPlayer: "black",
            };

            const position2: PositionState = {
                board: {
                    "11": { type: "king", owner: "black", promoted: false },
                } as Board,
                hands: initialHands(),
                currentPlayer: "white",
            };

            expect(hashPosition(position1)).not.toBe(hashPosition(position2));
        });

        it("持ち駒が異なると異なるハッシュ値を返す", () => {
            const hands1 = initialHands();
            const hands2 = initialHands();
            hands2.black.歩 = 1;

            const position1: PositionState = {
                board: {} as Board,
                hands: hands1,
                currentPlayer: "black",
            };

            const position2: PositionState = {
                board: {} as Board,
                hands: hands2,
                currentPlayer: "black",
            };

            expect(hashPosition(position1)).not.toBe(hashPosition(position2));
        });
    });

    describe("checkSennichite", () => {
        it("同一局面が4回出現したら千日手と判定する", () => {
            // 完全な空の盤面を作成
            const createEmptyBoard = (): Board => {
                const board: Board = {} as Board;
                for (let row = 1; row <= 9; row++) {
                    for (let col = 1; col <= 9; col++) {
                        board[`${row}${col}` as SquareKey] = null;
                    }
                }
                return board;
            };

            // 同じ盤面・持ち駒・手番の局面を独立して作成
            const createPosition = (player: Player): PositionState => {
                const board = createEmptyBoard();
                board["11"] = { type: "king", owner: "black", promoted: false };
                board["19"] = { type: "king", owner: "white", promoted: false };
                return {
                    board,
                    hands: {
                        black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
                        white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
                    },
                    currentPlayer: player,
                };
            };

            // 黒番の同じ局面が4回出現するケース（最低8つの局面が必要）
            const positionHistory: PositionState[] = [
                createPosition("black"), // 1回目 (黒番)
                createPosition("white"), // 白番
                createPosition("black"), // 2回目 (黒番)
                createPosition("white"), // 白番
                createPosition("black"), // 3回目 (黒番)
                createPosition("white"), // 白番
                createPosition("black"), // 4回目 (黒番)
                createPosition("white"), // 白番（8つ目）
            ];

            expect(checkSennichite(positionHistory)).toBe(true);
        });

        it("同一局面が3回以下の場合は千日手ではない", () => {
            const position: PositionState = {
                board: {
                    "11": { type: "king", owner: "black", promoted: false },
                } as Board,
                hands: initialHands(),
                currentPlayer: "black",
            };

            const positionHistory: PositionState[] = [
                position,
                { ...position, currentPlayer: "white" },
                position,
                { ...position, currentPlayer: "white" },
                position, // 3回目
            ];

            expect(checkSennichite(positionHistory)).toBe(false);
        });

        it("履歴が8手未満の場合は千日手ではない", () => {
            const positionHistory: PositionState[] = [
                {
                    board: {} as Board,
                    hands: initialHands(),
                    currentPlayer: "black",
                },
            ];

            expect(checkSennichite(positionHistory)).toBe(false);
        });
    });

    describe("checkPerpetualCheck", () => {
        it("同一局面4回すべてで王手の場合は連続王手の千日手", () => {
            // 完全な空の盤面を作成
            const createEmptyBoard = (): Board => {
                const board: Board = {} as Board;
                for (let row = 1; row <= 9; row++) {
                    for (let col = 1; col <= 9; col++) {
                        board[`${row}${col}` as SquareKey] = null;
                    }
                }
                return board;
            };

            // 同じ盤面・持ち駒・手番の局面を独立して作成
            const createPosition = (player: Player): PositionState => {
                const board = createEmptyBoard();
                board["55"] = { type: "king", owner: "white", promoted: false };
                board["54"] = { type: "rook", owner: "black", promoted: false }; // 王手をかけている飛車
                board["59"] = { type: "king", owner: "black", promoted: false };
                return {
                    board,
                    hands: {
                        black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
                        white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
                    },
                    currentPlayer: player,
                };
            };

            const positionHistory: PositionState[] = [
                createPosition("black"), // 1回目 (黒番)
                createPosition("white"), // 白番
                createPosition("black"), // 2回目 (黒番)
                createPosition("white"), // 白番
                createPosition("black"), // 3回目 (黒番)
                createPosition("white"), // 白番
                createPosition("black"), // 4回目 (黒番)
                createPosition("white"), // 白番（8つ目）
            ];

            const checkHistory = [true, false, true, false, true, false, true, false]; // 黒番の時はすべて王手

            expect(checkPerpetualCheck(positionHistory, checkHistory)).toBe(true);
        });

        it("同一局面4回だが王手でない場合は連続王手の千日手ではない", () => {
            const position: PositionState = {
                board: {} as Board,
                hands: initialHands(),
                currentPlayer: "black",
            };

            const positionHistory: PositionState[] = [
                position,
                { ...position, currentPlayer: "white" },
                position,
                { ...position, currentPlayer: "white" },
                position,
                { ...position, currentPlayer: "white" },
                position, // 4回目
            ];

            const checkHistory = [false, false, false, false, false, false, false];

            expect(checkPerpetualCheck(positionHistory, checkHistory)).toBe(false);
        });
    });

    describe("checkJishogi", () => {
        it("両玉が敵陣にいない場合は持将棋ではない", () => {
            const board: Board = {
                "59": { type: "king", owner: "black", promoted: false }, // 先手玉は自陣
                "51": { type: "king", owner: "white", promoted: false }, // 後手玉も自陣
            } as Board;

            const result = checkJishogi(board, initialHands());
            expect(result.isJishogi).toBe(false);
        });

        it("両玉が敵陣にいるが点数不足の場合は持将棋ではない", () => {
            const board: Board = {
                "11": { type: "king", owner: "black", promoted: false }, // 先手玉は敵陣
                "99": { type: "king", owner: "white", promoted: false }, // 後手玉も敵陣
            } as Board;

            const result = checkJishogi(board, initialHands());
            expect(result.isJishogi).toBe(false);
            expect(result.blackPoints).toBe(0);
            expect(result.whitePoints).toBe(0);
        });

        it("両玉が敵陣にいて点数条件を満たす場合は持将棋", () => {
            const board: Board = {
                "11": { type: "king", owner: "black", promoted: false }, // 先手玉は敵陣
                "99": { type: "king", owner: "white", promoted: false }, // 後手玉も敵陣
                // 先手の駒（24点以上必要）
                "12": { type: "rook", owner: "black", promoted: false }, // 5点
                "13": { type: "bishop", owner: "black", promoted: false }, // 5点
                "21": { type: "gold", owner: "black", promoted: false }, // 1点
                "22": { type: "gold", owner: "black", promoted: false }, // 1点
                "23": { type: "gold", owner: "black", promoted: false }, // 1点
                "31": { type: "gold", owner: "black", promoted: false }, // 1点
                // 合計14点
                // 後手の駒（27点以上必要）
                "91": { type: "rook", owner: "white", promoted: false }, // 5点
                "92": { type: "rook", owner: "white", promoted: false }, // 5点
                "93": { type: "bishop", owner: "white", promoted: false }, // 5点
                "81": { type: "bishop", owner: "white", promoted: false }, // 5点
                "82": { type: "gold", owner: "white", promoted: false }, // 1点
                "83": { type: "gold", owner: "white", promoted: false }, // 1点
                "71": { type: "gold", owner: "white", promoted: false }, // 1点
                "72": { type: "gold", owner: "white", promoted: false }, // 1点
                "73": { type: "gold", owner: "white", promoted: false }, // 1点
                "61": { type: "gold", owner: "white", promoted: false }, // 1点
                "62": { type: "gold", owner: "white", promoted: false }, // 1点
                // 合計27点
            } as Board;

            const hands = initialHands();
            // 先手の持ち駒
            hands.black.金 = 10; // 10点追加
            // 合計24点

            const result = checkJishogi(board, hands);
            expect(result.isJishogi).toBe(true);
            expect(result.blackPoints).toBe(24);
            expect(result.whitePoints).toBe(27);
        });

        it("駒の点数計算が正しい", () => {
            const board: Board = {
                "11": { type: "king", owner: "black", promoted: false },
                "99": { type: "king", owner: "white", promoted: false },
                "12": { type: "rook", owner: "black", promoted: false }, // 5点
                "13": { type: "bishop", owner: "black", promoted: false }, // 5点
                "14": { type: "gold", owner: "black", promoted: false }, // 1点
                "15": { type: "silver", owner: "black", promoted: false }, // 1点
                "16": { type: "knight", owner: "black", promoted: false }, // 1点
                "17": { type: "lance", owner: "black", promoted: false }, // 1点
                "18": { type: "pawn", owner: "black", promoted: false }, // 1点
            } as Board;

            const hands = initialHands();
            hands.black.飛 = 1; // 5点
            hands.black.角 = 1; // 5点
            hands.black.歩 = 5; // 5点

            const result = checkJishogi(board, hands);
            expect(result.blackPoints).toBe(30); // 15点（盤上）+ 15点（持ち駒）
        });
    });

    describe("checkMaxMoves", () => {
        it("手数が最大手数に達したら引き分けと判定", () => {
            expect(checkMaxMoves(512)).toBe(true);
            expect(checkMaxMoves(513)).toBe(true);
        });

        it("手数が最大手数未満なら引き分けではない", () => {
            expect(checkMaxMoves(511)).toBe(false);
            expect(checkMaxMoves(0)).toBe(false);
        });

        it("カスタム最大手数を設定できる", () => {
            expect(checkMaxMoves(100, 100)).toBe(true);
            expect(checkMaxMoves(99, 100)).toBe(false);
        });
    });

    describe("checkTryRule", () => {
        it("先手玉が5一に到達したらトライ勝ち", () => {
            const board: Board = {
                "51": { type: "king", owner: "black", promoted: false },
                "99": { type: "king", owner: "white", promoted: false },
            } as Board;

            expect(checkTryRule(board, "black")).toBe(true);
        });

        it("後手玉が5九に到達したらトライ勝ち", () => {
            const board: Board = {
                "11": { type: "king", owner: "black", promoted: false },
                "59": { type: "gyoku", owner: "white", promoted: false },
            } as Board;

            expect(checkTryRule(board, "white")).toBe(true);
        });

        it("相手の玉が目標地点にいてもトライではない", () => {
            const board: Board = {
                "51": { type: "king", owner: "white", promoted: false }, // 後手玉が5一
                "99": { type: "king", owner: "black", promoted: false },
            } as Board;

            expect(checkTryRule(board, "black")).toBe(false);
        });

        it("目標地点に他の駒がいる場合はトライではない", () => {
            const board: Board = {
                "51": { type: "gold", owner: "black", promoted: false }, // 金が5一
                "99": { type: "king", owner: "black", promoted: false },
                "11": { type: "king", owner: "white", promoted: false },
            } as Board;

            expect(checkTryRule(board, "black")).toBe(false);
        });

        it("目標地点が空の場合はトライではない", () => {
            const board: Board = {
                "99": { type: "king", owner: "black", promoted: false },
                "11": { type: "king", owner: "white", promoted: false },
            } as Board;

            expect(checkTryRule(board, "black")).toBe(false);
            expect(checkTryRule(board, "white")).toBe(false);
        });
    });
});
