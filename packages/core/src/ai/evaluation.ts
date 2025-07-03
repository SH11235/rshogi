import type { Board } from "../domain/model/board";
import type { Piece, PieceType, Player } from "../domain/model/piece";
import type { Square, SquareKey } from "../domain/model/square";
import { isInCheck } from "../domain/service/checkmate";
import type { Hands } from "../domain/service/moveService";
import { generateMoves } from "../domain/service/moveService";

// 駒の基本価値（センチポーン単位）
export const PIECE_BASE_VALUES: Record<PieceType, number> = {
    pawn: 100,
    lance: 430,
    knight: 450,
    silver: 640,
    gold: 690,
    bishop: 890,
    rook: 1040,
    king: 0, // 王は価値計算に含めない
    gyoku: 0,
};

// 成り駒のボーナス
export const PROMOTION_BONUS: Record<PieceType, number> = {
    pawn: 420, // と金は歩より価値が高い
    lance: 260,
    knight: 240,
    silver: 50,
    gold: 0,
    bishop: 150,
    rook: 150,
    king: 0,
    gyoku: 0,
};

// 駒ごとの位置評価テーブル（先手視点）
const PIECE_SQUARE_TABLES: Record<PieceType, number[]> = {
    // 歩兵: 前進を推奨、5筋を重視
    pawn: [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 15, 15, 15, 20, 25, 20, 15, 15, 15, 10, 10, 10, 15, 20, 15, 10,
        10, 10, 5, 5, 5, 10, 15, 10, 5, 5, 5, 0, 0, 0, 5, 10, 5, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0,
        -5, -5, -5, -5, -5, -5, -5, -5, -5, -10, -10, -10, -10, -10, -10, -10, -10, -10, 0, 0, 0, 0,
        0, 0, 0, 0, 0,
    ],
    // 香車: 前進を推奨、端筋を重視
    lance: [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 5, 5, 5, 5, 5, 5, 5, 10, 5, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -5, -5, -5, -5, -5, -5, -5, -5, -5, -10, -10,
        -10, -10, -10, -10, -10, -10, -10, -15, -15, -15, -15, -15, -15, -15, -15, -15, -20, -20,
        -20, -20, -20, -20, -20, -20, -20,
    ],
    // 桂馬: 前進と中央を推奨
    knight: [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 15, 20, 20, 20, 20, 20, 15, 10, 5, 10, 15, 15, 15, 15, 15,
        10, 5, 0, 5, 10, 10, 10, 10, 10, 5, 0, -5, 0, 5, 5, 5, 5, 5, 0, -5, -10, -5, 0, 0, 0, 0, 0,
        -5, -10, -15, -10, -5, -5, -5, -5, -5, -10, -15, -20, -15, -10, -10, -10, -10, -10, -15,
        -20, -30, -30, -30, -30, -30, -30, -30, -30, -30,
    ],
    // 銀: 前進と中央を推奨
    silver: [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 15, 15, 15, 15, 15, 15, 15, 10, 10, 20, 20, 20, 20, 20, 20,
        20, 10, 5, 15, 25, 25, 25, 25, 25, 15, 5, 0, 10, 20, 20, 20, 20, 20, 10, 0, -5, 5, 10, 10,
        10, 10, 10, 5, -5, -10, 0, 5, 5, 5, 5, 5, 0, -10, -15, -5, 0, 0, 0, 0, 0, -5, -15, -20, -10,
        -5, -5, -5, -5, -5, -10, -20,
    ],
    // 金: 王の近くを推奨
    gold: [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 10, 10, 10, 10, 10, 10, 10, 5, 5, 10, 15, 15, 15, 15, 15, 10,
        5, 0, 5, 10, 10, 10, 10, 10, 5, 0, 0, 5, 10, 10, 10, 10, 10, 5, 0, 0, 5, 10, 10, 10, 10, 10,
        5, 0, 5, 10, 15, 15, 15, 15, 15, 10, 5, 10, 15, 20, 20, 20, 20, 20, 15, 10, 15, 20, 25, 25,
        25, 25, 25, 20, 15,
    ],
    // 角: 中央と対角線を重視
    bishop: [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 5, 5, 5, 5, 5, 10, 0, 0, 5, 15, 10, 10, 10, 15, 5, 0, 0,
        5, 10, 20, 15, 20, 10, 5, 0, 0, 5, 10, 15, 25, 15, 10, 5, 0, 0, 5, 10, 20, 15, 20, 10, 5, 0,
        0, 5, 15, 10, 10, 10, 15, 5, 0, 0, 10, 5, 5, 5, 5, 5, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    // 飛車: 中央と直線を重視
    rook: [
        5, 5, 5, 10, 10, 10, 5, 5, 5, 10, 10, 10, 15, 15, 15, 10, 10, 10, 0, 0, 0, 5, 5, 5, 0, 0, 0,
        0, 0, 0, 5, 5, 5, 0, 0, 0, 0, 0, 0, 5, 5, 5, 0, 0, 0, 0, 0, 0, 5, 5, 5, 0, 0, 0, 0, 0, 0, 5,
        5, 5, 0, 0, 0, 20, 20, 20, 25, 25, 25, 20, 20, 20, 25, 25, 25, 30, 30, 30, 25, 25, 25,
    ],
    // 王: 序盤は城を組むことを推奨
    king: [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 5, 5, 0, 0, 0, 5, 5,
        5, 10, 10, 10, 5, 5, 5, 10, 10, 10, 20, 30, 20, 10, 10, 10, 20, 30, 20,
    ],
    gyoku: [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 5, 5, 0, 0, 0, 5, 5,
        5, 10, 10, 10, 5, 5, 5, 10, 10, 10, 20, 30, 20, 10, 10, 10, 20, 30, 20,
    ],
};

// 駒の利きをカウント
export function countAttacks(board: Board, square: Square, player: Player): number {
    let attacks = 0;

    // 全ての自分の駒からの利きをチェック
    for (const [from, piece] of Object.entries(board)) {
        if (!piece || (piece as Piece).owner !== player) continue;

        const fromSquare = {
            row: Number.parseInt(from[0]) as Square["row"],
            column: Number.parseInt(from[1]) as Square["column"],
        };

        // この駒から目標のマスへの利きをチェック
        const moves = generateMoves(board, fromSquare);
        if (moves.some((move) => move.to.row === square.row && move.to.column === square.column)) {
            attacks++;
        }
    }

    return attacks;
}

// 王の安全性評価
export function evaluateKingSafety(board: Board, player: Player): number {
    let safety = 0;

    // 王の位置を探す
    let kingSquare: Square | null = null;
    for (const [key, piece] of Object.entries(board)) {
        if (
            piece &&
            ((piece as Piece).type === "king" || (piece as Piece).type === "gyoku") &&
            (piece as Piece).owner === player
        ) {
            kingSquare = {
                row: Number.parseInt(key[0]) as Square["row"],
                column: Number.parseInt(key[1]) as Square["column"],
            };
            break;
        }
    }

    if (!kingSquare) return -10000; // 王がいない場合は大きなペナルティ

    // 王手されている場合のペナルティ
    if (isInCheck(board, player)) {
        safety -= 50;
    }

    // 王の周囲の守り駒をカウント
    const directions = [
        [-1, -1],
        [-1, 0],
        [-1, 1],
        [0, -1],
        [0, 1],
        [1, -1],
        [1, 0],
        [1, 1],
    ];

    for (const [dr, dc] of directions) {
        const r = kingSquare.row + dr;
        const c = kingSquare.column + dc;

        if (r >= 1 && r <= 9 && c >= 1 && c <= 9) {
            const piece = board[`${r}${c}` as SquareKey];
            if (piece && (piece as Piece).owner === player) {
                // 味方の駒による守り
                if ((piece as Piece).type === "gold" || (piece as Piece).promoted) {
                    safety += 15; // 金と成り駒は良い守り
                } else if ((piece as Piece).type === "silver") {
                    safety += 10;
                } else {
                    safety += 5;
                }
            }
        }
    }

    // 敵の駒からの攻撃をチェック
    const enemyPlayer = player === "black" ? "white" : "black";
    const enemyAttacks = countAttacks(board, kingSquare, enemyPlayer);
    safety -= enemyAttacks * 20;

    return safety;
}

// 駒の活動性評価（利きの数）
export function evaluateMobility(board: Board, player: Player): number {
    let mobility = 0;

    for (const [from, piece] of Object.entries(board)) {
        if (!piece || (piece as Piece).owner !== player) continue;

        const fromSquare = {
            row: Number.parseInt(from[0]) as Square["row"],
            column: Number.parseInt(from[1]) as Square["column"],
        };

        const moves = generateMoves(board, fromSquare);

        // 駒種別の重み付け
        const weight =
            (piece as Piece).type === "rook" || (piece as Piece).type === "bishop" ? 2 : 1;
        mobility += moves.length * weight;
    }

    return mobility;
}

// 駒の連携評価
export function evaluatePieceCoordination(board: Board, player: Player): number {
    let coordination = 0;

    // 各マスについて、複数の味方駒から利きがある場合にボーナス
    for (let row = 1; row <= 9; row++) {
        for (let col = 1; col <= 9; col++) {
            const square: Square = { row: row as Square["row"], column: col as Square["column"] };
            const attacks = countAttacks(board, square, player);

            if (attacks >= 2) {
                coordination += (attacks - 1) * 5; // 複数の利きがある場合のボーナス
            }
        }
    }

    return coordination;
}

// 位置評価
export function getPositionBonus(piece: Piece, square: SquareKey): number {
    const row = Number.parseInt(square[0]);
    const col = Number.parseInt(square[1]);
    const index = (row - 1) * 9 + (col - 1);

    // 後手の場合はテーブルを反転
    const tableIndex = piece.owner === "black" ? index : 80 - index;

    const table = PIECE_SQUARE_TABLES[piece.type];
    return table ? table[tableIndex] || 0 : 0;
}

// 総合評価関数
export function evaluatePosition(
    board: Board,
    hands: Hands,
    player: Player,
): {
    material: number;
    position: number;
    kingSafety: number;
    mobility: number;
    coordination: number;
    total: number;
} {
    let material = 0;
    let position = 0;

    // 盤上の駒の評価
    for (const [square, piece] of Object.entries(board)) {
        if (!piece) continue;

        const baseValue = PIECE_BASE_VALUES[(piece as Piece).type];
        const promotionBonus = (piece as Piece).promoted
            ? PROMOTION_BONUS[(piece as Piece).type]
            : 0;
        const positionBonus = getPositionBonus(piece as Piece, square as SquareKey);

        if ((piece as Piece).owner === player) {
            material += baseValue + promotionBonus;
            position += positionBonus;
        } else {
            material -= baseValue + promotionBonus;
            position -= positionBonus;
        }
    }

    // 持ち駒の評価
    const handPieces: PieceType[] = ["pawn", "lance", "knight", "silver", "gold", "bishop", "rook"];
    for (const pieceType of handPieces) {
        const blackCount = hands.black[pieceType] || 0;
        const whiteCount = hands.white[pieceType] || 0;
        const pieceValue = PIECE_BASE_VALUES[pieceType] * 0.9; // 持ち駒は少し価値を下げる

        if (player === "black") {
            material += blackCount * pieceValue;
            material -= whiteCount * pieceValue;
        } else {
            material += whiteCount * pieceValue;
            material -= blackCount * pieceValue;
        }
    }

    // その他の評価要素
    const kingSafety =
        evaluateKingSafety(board, player) -
        evaluateKingSafety(board, player === "black" ? "white" : "black");
    const mobility =
        evaluateMobility(board, player) -
        evaluateMobility(board, player === "black" ? "white" : "black");
    const coordination =
        evaluatePieceCoordination(board, player) -
        evaluatePieceCoordination(board, player === "black" ? "white" : "black");

    // 重み付けして合計
    const total = material + position + kingSafety * 2 + mobility * 0.1 + coordination * 0.5;

    return {
        material,
        position,
        kingSafety,
        mobility,
        coordination,
        total,
    };
}
