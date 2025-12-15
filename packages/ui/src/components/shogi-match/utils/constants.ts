import type { PieceType } from "@shogi/app-core";

/**
 * 駒の種類ごとの上限枚数
 */
export const PIECE_CAP: Record<PieceType, number> = {
    P: 18,
    L: 4,
    N: 4,
    S: 4,
    G: 4,
    B: 2,
    R: 2,
    K: 1,
};

/**
 * 駒の種類ごとの日本語ラベル
 */
export const PIECE_LABELS: Record<PieceType, string> = {
    K: "玉",
    R: "飛",
    B: "角",
    G: "金",
    S: "銀",
    N: "桂",
    L: "香",
    P: "歩",
};

/**
 * 駒が成れるかどうかを判定する
 *
 * @param type - 駒の種類
 * @returns 成れる場合は true
 */
export function isPromotable(type: PieceType): boolean {
    return type !== "K" && type !== "G";
}
