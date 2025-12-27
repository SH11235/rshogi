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

/**
 * マス内座標表示の形式
 * - 'none': 非表示
 * - 'sfen': SFEN形式 (例: 5e)
 * - 'japanese': 日本式 (例: ５五)
 */
export type SquareNotation = "none" | "sfen" | "japanese";

/**
 * 盤面表示設定
 */
export interface DisplaySettings {
    /** マス内座標表示形式 */
    squareNotation: SquareNotation;
    /** 盤外ラベル（筋・段）表示 */
    showBoardLabels: boolean;
    /** 最終手ハイライト */
    highlightLastMove: boolean;
    /** 棋譜パネルに評価値を表示 */
    showKifuEval: boolean;
    /** マウスホイールで棋譜をナビゲート */
    enableWheelNavigation: boolean;
}

/** デフォルト表示設定 */
export const DEFAULT_DISPLAY_SETTINGS: DisplaySettings = {
    squareNotation: "none",
    showBoardLabels: true,
    highlightLastMove: true,
    showKifuEval: false,
    enableWheelNavigation: true,
};
