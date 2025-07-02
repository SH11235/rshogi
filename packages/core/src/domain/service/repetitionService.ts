import type { Board } from "../model/board";
import type { Player } from "../model/piece";
import type { Hands } from "./moveService";

/**
 * 局面の状態を表すインターフェース
 */
export interface PositionState {
    board: Board;
    hands: Hands;
    currentPlayer: Player;
}

/**
 * 局面をハッシュ化して比較可能な文字列に変換
 */
export function hashPosition(position: PositionState): string {
    // 盤面の状態をハッシュ化
    const boardHash = Object.entries(position.board)
        .filter(([_, piece]) => piece !== null)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([key, piece]) => `${key}:${piece?.type}${piece?.owner}${piece?.promoted ? "+" : ""}`)
        .join(",");

    // 持ち駒の状態をハッシュ化
    const handsHash = ["black", "white"]
        .map((player) => {
            const playerHands = position.hands[player as Player];
            return Object.entries(playerHands)
                .filter(([_, count]) => count > 0)
                .sort(([a], [b]) => a.localeCompare(b))
                .map(([piece, count]) => `${player}:${piece}:${count}`)
                .join(",");
        })
        .filter((h) => h.length > 0)
        .join(",");

    // 手番を含めてハッシュ化
    return `${position.currentPlayer}|${boardHash}|${handsHash}`;
}

/**
 * 千日手の判定
 * 同一局面が4回出現したら千日手
 */
export function checkSennichite(positionHistory: PositionState[]): boolean {
    if (positionHistory.length < 8) {
        // 最低でも8手必要（同じ局面が4回出現するため）
        return false;
    }

    // 各局面の出現回数をカウント
    const positionCounts = new Map<string, number>();

    for (const position of positionHistory) {
        const hash = hashPosition(position);
        const count = (positionCounts.get(hash) || 0) + 1;
        positionCounts.set(hash, count);

        if (count >= 4) {
            return true;
        }
    }

    return false;
}

/**
 * 連続王手の千日手かどうかを判定
 * 同一局面が4回出現し、そのすべてで王手がかかっている場合
 */
export function checkPerpetualCheck(
    positionHistory: PositionState[],
    checkHistory: boolean[],
): boolean {
    if (positionHistory.length < 8 || checkHistory.length !== positionHistory.length) {
        return false;
    }

    // 各局面とその王手状態を記録
    const positionCheckCounts = new Map<string, { count: number; checks: number }>();

    for (let i = 0; i < positionHistory.length; i++) {
        const hash = hashPosition(positionHistory[i]);
        const isCheck = checkHistory[i];

        const data = positionCheckCounts.get(hash) || { count: 0, checks: 0 };
        data.count++;
        if (isCheck) {
            data.checks++;
        }
        positionCheckCounts.set(hash, data);

        // 同一局面が4回出現し、すべて王手の場合
        if (data.count >= 4 && data.count === data.checks) {
            return true;
        }
    }

    return false;
}

/**
 * 持将棋（入玉）の判定
 * 両玉が敵陣に入り、互いに詰みがない状態
 */
export interface JishogiStatus {
    isJishogi: boolean;
    blackPoints?: number;
    whitePoints?: number;
}

/**
 * 駒の点数計算（持将棋用）
 * 大駒（飛車・角）: 5点
 * その他の駒（玉以外）: 1点
 */
function calculatePiecePoints(board: Board, hands: Hands, player: Player): number {
    let points = 0;

    // 盤上の駒の点数
    for (const piece of Object.values(board)) {
        if (piece && piece.owner === player && piece.type !== "king" && piece.type !== "gyoku") {
            if (piece.type === "rook" || piece.type === "bishop") {
                points += 5;
            } else {
                points += 1;
            }
        }
    }

    // 持ち駒の点数
    const playerHands = hands[player];
    for (const [pieceName, count] of Object.entries(playerHands)) {
        if (count > 0) {
            if (pieceName === "飛" || pieceName === "角") {
                points += 5 * count;
            } else {
                points += count;
            }
        }
    }

    return points;
}

/**
 * 玉が敵陣（相手の3段目以内）に入っているか判定
 */
function isKingInEnemyTerritory(board: Board, player: Player): boolean {
    const targetRows = player === "black" ? [1, 2, 3] : [7, 8, 9];

    for (const [key, piece] of Object.entries(board)) {
        if (piece && piece.owner === player && (piece.type === "king" || piece.type === "gyoku")) {
            const row = Number.parseInt(key[0]);
            if (targetRows.includes(row)) {
                return true;
            }
        }
    }

    return false;
}

/**
 * 持将棋の判定
 */
export function checkJishogi(board: Board, hands: Hands): JishogiStatus {
    // 両玉が敵陣に入っているか確認
    const blackKingInEnemy = isKingInEnemyTerritory(board, "black");
    const whiteKingInEnemy = isKingInEnemyTerritory(board, "white");

    if (!blackKingInEnemy || !whiteKingInEnemy) {
        return { isJishogi: false };
    }

    // 点数計算
    const blackPoints = calculatePiecePoints(board, hands, "black");
    const whitePoints = calculatePiecePoints(board, hands, "white");

    // 入玉宣言法による判定
    // 両者が敵陣に玉を入れた状態で、先手が24点以上、後手が27点以上なら持将棋
    const isJishogi = blackPoints >= 24 && whitePoints >= 27;

    return {
        isJishogi,
        blackPoints,
        whitePoints,
    };
}

/**
 * 手数による引き分け判定（最大手数）
 */
export function checkMaxMoves(moveCount: number, maxMoves = 512): boolean {
    return moveCount >= maxMoves;
}

/**
 * トライルール判定
 * 自分の玉が相手の玉の初期位置（後手なら59、先手なら51）に到達したら勝利
 */
export function checkTryRule(board: Board, currentPlayer: Player): boolean {
    // 相手の玉の初期位置
    const targetSquare = currentPlayer === "black" ? "51" : "59";

    // 対象のマスにある駒を確認
    const piece = board[targetSquare];

    // 自分の玉が対象マスにいるか判定
    return (
        piece !== null &&
        piece !== undefined &&
        piece.owner === currentPlayer &&
        (piece.type === "king" || piece.type === "gyoku")
    );
}
