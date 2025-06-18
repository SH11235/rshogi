/**
 * 日本語棋譜記法の型定義
 */

/** 先手・後手のプレフィックス */
export type PlayerPrefix = "☗" | "☖";

/** 日本語棋譜記法の手順文字列 */
export type MoveNotation = `${PlayerPrefix}${number}. ${string}`;

/** プロモーションダイアログの状態 */
export interface PromotionPendingMove {
    from: Square;
    to: Square;
    piece: Piece;
}

import type { Piece } from "./piece";
import type { Square } from "./square";
