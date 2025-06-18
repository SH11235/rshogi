/**
 * 履歴カーソルの定数値
 *
 * historyCursor の値の意味:
 * - LATEST_POSITION: 最新位置（全ての手が指された後の状態）
 * - INITIAL_POSITION: 初期位置（開始局面、手が指される前の状態）
 * - 0以上の数値: 指定されたインデックスの手が指された後の状態（0 = 1手目後, 1 = 2手目後, ...）
 */
export const HISTORY_CURSOR = {
    /** 最新位置（全ての手が指された後） */
    LATEST_POSITION: -1,
    /** 初期位置（開始局面） */
    INITIAL_POSITION: -2,
} as const;

export type HistoryCursor = (typeof HISTORY_CURSOR)[keyof typeof HISTORY_CURSOR] | number;
