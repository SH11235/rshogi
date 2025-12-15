import type { Piece, Square } from "@shogi/app-core";

/**
 * 成り判定の結果を表す型
 * - 'none': 成れない（成り手が存在しない）
 * - 'optional': 任意成り（基本移動と成り移動の両方が合法）
 * - 'forced': 強制成り（成り移動のみ合法）
 */
export type PromotionDecision = "none" | "optional" | "forced";

/**
 * 成り選択ダイアログの状態を表す型
 */
export type PromotionSelection = {
    from: Square;
    to: Square;
    piece: Piece; // 駒情報を追加（UI表示・検証用）
};
