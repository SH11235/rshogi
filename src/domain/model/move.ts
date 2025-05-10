import type { Piece } from "./piece";
import type { Square } from "./square";

/**
 * 通常の指し手（盤上の駒を動かす）
 * - from: 出発地点
 * - to:   到着地点
 * - promote: 今回の手で「成り」を選択したか
 * - captured: 取った駒（なければ null）
 */
export type NormalMove = {
    type: "move";
    from: Square;
    to: Square;
    piece: Piece; // 動かす前の駒（promoted=false の場合あり）
    promote: boolean;
    captured: Piece | null;
};

/**
 * 打ち手（持ち駒を盤上に置く）
 * - piece は必ず promoted=false の駒
 */
export type DropMove = {
    type: "drop";
    to: Square;
    piece: Piece; // owner は手番側、promoted は常に false
};

export type Move = NormalMove | DropMove;
