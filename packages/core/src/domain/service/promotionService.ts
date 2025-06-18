import type { Piece } from "../model/piece";
import type { Square } from "../model/square";

/**
 * 位置に基づく成り可能判定のヘルパー関数
 *
 * @param piece 移動する駒
 * @param from 移動元の位置
 * @param to 移動先の位置
 * @returns 成ることができるかどうか
 */
export function canPromoteByPosition(piece: Piece, from: Square, to: Square): boolean {
    // 既に成っている駒は成れない
    if (piece.promoted) return false;

    // 王は成れない
    if (piece.type === "king" || piece.type === "gyoku") return false;

    // 金は成れない
    if (piece.type === "gold") return false;

    // 成り可能な条件：敵陣に入る、敵陣から移動する、敵陣内で移動する
    const isBlack = piece.owner === "black";
    const enemyZone = isBlack ? [1, 2, 3] : [7, 8, 9];

    const fromInEnemyZone = enemyZone.includes(from.row);
    const toInEnemyZone = enemyZone.includes(to.row);

    return fromInEnemyZone || toInEnemyZone;
}

/**
 * 成りを強制される場合の判定
 *
 * @param piece 移動する駒
 * @param to 移動先の位置
 * @returns 成りが強制されるかどうか
 */
export function mustPromote(piece: Piece, to: Square): boolean {
    if (piece.promoted) return false;

    const isBlack = piece.owner === "black";

    // 歩、香：最奥段で行き場がなくなる
    if (piece.type === "pawn" || piece.type === "lance") {
        return isBlack ? to.row === 1 : to.row === 9;
    }

    // 桂：最奥2段で行き場がなくなる
    if (piece.type === "knight") {
        return isBlack ? to.row <= 2 : to.row >= 8;
    }

    return false;
}
