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

/**
 * 自動解析モード
 * - 'off': 手動で解析ボタンを押したときのみ
 * - 'delayed': 操作が落ち着いてから（デフォルト、省電力）
 * - 'immediate': 分岐作成と同時に解析開始（電池消費大）
 */
export type AutoAnalyzeMode = "off" | "delayed" | "immediate";

/**
 * 解析設定
 */
export interface AnalysisSettings {
    /** 並列解析のワーカー数（0=自動検出） */
    parallelWorkers: number;
    /** 一括解析時の1手あたり解析時間(ms) */
    batchAnalysisTimeMs: number;
    /** 一括解析時の探索深さ */
    batchAnalysisDepth: number;
    /** 分岐作成時の自動解析モード */
    autoAnalyzeMode: AutoAnalyzeMode;
    /** 候補手数（MultiPV）、デフォルト: 1 */
    multiPv: number;
}

/** デフォルト解析設定 */
export const DEFAULT_ANALYSIS_SETTINGS: AnalysisSettings = {
    parallelWorkers: 0, // 0 = 自動検出
    batchAnalysisTimeMs: 1000,
    batchAnalysisDepth: 15,
    autoAnalyzeMode: "delayed",
    multiPv: 1,
};

/**
 * ゲームモード
 * - 'editing': 盤面編集モード（駒の配置・削除）
 * - 'playing': 対局モード（手番に従って指し手を進める）
 * - 'reviewing': 検討モード（自由に分岐を作成）
 */
export type GameMode = "editing" | "playing" | "reviewing";

/**
 * 解析状態を表す型
 * - 'none': 解析していない
 * - 'by-ply': 通常解析（plyで評価値を保存）
 * - 'by-node-id': 分岐解析（ノードIDで評価値を保存）
 */
export type AnalyzingState =
    | { type: "none" }
    | { type: "by-ply"; ply: number }
    | { type: "by-node-id"; nodeId: string; ply: number };

/** 解析していない状態の定数 */
export const ANALYZING_STATE_NONE: AnalyzingState = { type: "none" };
