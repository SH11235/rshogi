import { modernInitialBoard } from "../initialBoard";
import type { Board } from "../model/board";
import { HISTORY_CURSOR } from "../model/history";
import type { Move } from "../model/move";
import type { Piece, Player } from "../model/piece";
import type { Square } from "../model/square";
import { applyMove, initialHands } from "./moveService";
import { determineGameStatus } from "./utils";

// 持ち駒の型定義（moveService から独立）
export type Hands = {
    black: Record<string, number>;
    white: Record<string, number>;
};

/**
 * シリアライゼーション対応のゲーム状態型定義
 * JSON安全でプラットフォーム非依存
 */

export type GameState = {
    readonly board: SerializableBoard;
    readonly hands: Hands;
    readonly currentPlayer: Player;
    readonly moveHistory: Move[];
    readonly gameStatus: GameStatus;
    readonly metadata: GameMetadata;
};

export type SerializableBoard = {
    readonly pieces: SerializablePiece[];
};

export type SerializablePiece = {
    readonly position: Square;
    readonly type: Piece["type"];
    readonly promoted: boolean;
    readonly owner: Player;
};

export type GameStatus =
    | "playing"
    | "check"
    | "checkmate"
    | "draw"
    | "sennichite"
    | "perpetual_check"
    | "timeout"
    | "resigned"
    | "black_win"
    | "white_win"
    | "try_rule_black"
    | "try_rule_white";

export type GameMetadata = {
    readonly gameId: string;
    readonly startTime: string; // ISO 8601 string
    readonly lastMoveTime: string; // ISO 8601 string
    readonly blackPlayer?: string;
    readonly whitePlayer?: string;
    readonly timeControl?: TimeControl;
};

export type TimeControl = {
    readonly initialTime: number; // seconds
    readonly increment: number; // seconds per move
    readonly blackTime: number; // remaining seconds
    readonly whiteTime: number; // remaining seconds
};

/**
 * ゲーム状態の変換ユーティリティ
 */

// Board -> SerializableBoard 変換
export const serializeBoard = (board: Board): SerializableBoard => {
    const pieces: SerializablePiece[] = [];

    for (const [positionKey, piece] of Object.entries(board)) {
        if (piece) {
            const position = parsePosition(positionKey);
            // 新しい統一された Piece 型をシリアライズ
            pieces.push({
                position,
                type: piece.type,
                promoted: piece.promoted,
                owner: piece.owner,
            });
        }
    }

    return { pieces };
};

// SerializableBoard -> Board 変換
export const deserializeBoard = (serializableBoard: SerializableBoard): Board => {
    // 空の盤面から開始
    const board: Board = { ...modernInitialBoard };

    // 全マスを null で初期化
    for (let row = 1; row <= 9; row++) {
        for (let col = 1; col <= 9; col++) {
            const key = `${row}${col}` as keyof Board;
            board[key] = null;
        }
    }

    // 駒を配置
    for (const piece of serializableBoard.pieces) {
        const key = `${piece.position.row}${piece.position.column}` as keyof Board;
        board[key] = {
            type: piece.type,
            promoted: piece.promoted,
            owner: piece.owner,
        };
    }

    return board;
};

// Position文字列をパース
const parsePosition = (positionKey: string): Square => {
    const row = Number.parseInt(positionKey[0]) as Square["row"];
    const column = Number.parseInt(positionKey[1]) as Square["column"];
    return { row, column };
};

/**
 * ゲーム状態作成ヘルパー
 */

export const createInitialGameState = (
    gameId: string,
    blackPlayer?: string,
    whitePlayer?: string,
    timeControl?: TimeControl,
): GameState => {
    const now = new Date().toISOString();

    return {
        board: serializeBoard(modernInitialBoard), // 初期盤面から開始
        hands: {
            black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
            white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
        },
        currentPlayer: "black",
        moveHistory: [],
        gameStatus: "playing",
        metadata: {
            gameId,
            startTime: now,
            lastMoveTime: now,
            blackPlayer,
            whitePlayer,
            timeControl,
        },
    };
};

export const updateGameState = (
    currentState: GameState,
    move: Move,
    newBoard: Board,
    newHands: Hands,
    nextPlayer: Player,
    newStatus: GameStatus = "playing",
): GameState => {
    return {
        board: serializeBoard(newBoard),
        hands: newHands,
        currentPlayer: nextPlayer,
        moveHistory: [...currentState.moveHistory, move],
        gameStatus: newStatus,
        metadata: {
            ...currentState.metadata,
            lastMoveTime: new Date().toISOString(),
        },
    };
};

/**
 * ゲーム状態の検証
 */

export const isValidGameState = (gameState: unknown): gameState is GameState => {
    if (typeof gameState !== "object" || gameState === null) {
        return false;
    }

    const state = gameState as Record<string, unknown>;

    return (
        typeof state.board === "object" &&
        typeof state.hands === "object" &&
        (state.currentPlayer === "black" || state.currentPlayer === "white") &&
        Array.isArray(state.moveHistory) &&
        typeof state.gameStatus === "string" &&
        typeof state.metadata === "object"
    );
};

/**
 * JSONシリアライゼーション
 */

export const serializeGameState = (gameState: GameState): string => {
    return JSON.stringify(gameState);
};

export const deserializeGameState = (json: string): GameState => {
    const parsed = JSON.parse(json);

    if (!isValidGameState(parsed)) {
        throw new Error("Invalid game state format");
    }

    return parsed;
};

/**
 * ゲーム状態の比較（千日手判定用）
 */

export const compareGamePositions = (state1: GameState, state2: GameState): boolean => {
    return (
        JSON.stringify(state1.board) === JSON.stringify(state2.board) &&
        JSON.stringify(state1.hands) === JSON.stringify(state2.hands) &&
        state1.currentPlayer === state2.currentPlayer
    );
};

export const findRepetition = (gameState: GameState): number => {
    let count = 0;

    // 過去の局面と比較（簡易実装）
    for (let i = gameState.moveHistory.length - 1; i >= 0; i -= 2) {
        // 2手前ずつチェック（同じプレイヤーの手番）
        // 実際の実装では局面ハッシュを使った効率的な比較が必要
        count++;
        if (count >= 4) {
            // 4回繰り返しで千日手
            return 4;
        }
    }

    return count;
};

/**
 * 初期状態から指定した手数まで再構築する関数
 *
 * @param moveHistory 手の履歴
 * @param targetMoveIndex 再構築対象の手数インデックス
 * @returns 再構築されたゲーム状態
 */
export function reconstructGameState(moveHistory: Move[], targetMoveIndex: number) {
    return reconstructGameStateWithInitial(
        moveHistory,
        targetMoveIndex,
        modernInitialBoard,
        initialHands(),
    );
}

/**
 * 初期局面を考慮したゲーム状態再構築
 *
 * @param moveHistory 手の履歴
 * @param targetMoveIndex 再構築対象の手数インデックス
 * @param initialBoard 初期盤面
 * @param initialHandsData 初期持ち駒
 * @returns 再構築されたゲーム状態
 */
export function reconstructGameStateWithInitial(
    moveHistory: Move[],
    targetMoveIndex: number,
    initialBoard: Board,
    initialHandsData: Hands,
) {
    let board = structuredClone(initialBoard);
    let hands = structuredClone(initialHandsData);
    let currentPlayer: Player = "black";

    // 初期位置の場合は何も手を適用しない
    if (targetMoveIndex === HISTORY_CURSOR.INITIAL_POSITION) {
        // 初期状態をそのまま返す
    } else {
        for (let i = 0; i <= targetMoveIndex; i++) {
            if (i >= moveHistory.length) break;

            const result = applyMove(board, hands, currentPlayer, moveHistory[i]);
            board = result.board;
            hands = result.hands;
            currentPlayer = result.nextTurn;
        }
    }

    // ゲーム状態判定
    const gameStatus = determineGameStatus(board, hands, currentPlayer);

    return { board, hands, currentPlayer, gameStatus };
}
