/**
 * バイナリ形式の定跡データ型定義
 * より効率的なメモリ使用とファイルサイズの削減を実現
 */

/**
 * 駒の移動を表すバイナリ形式
 * 6バイトで1つの手を表現（JSONの約1/10のサイズ）
 */
export interface MoveBinary {
    from: number; // 移動元（0-80: 0=駒打ち、1-81=盤面位置）
    to: number; // 移動先（1-81: 盤面位置）
    flags: number; // フラグ（駒種類、成り、打ち）
    weight: number; // 重み（0-255）
    eval: number; // 評価値（-32768〜32767）
}

/**
 * フラグのビット構成（8ビット）
 * - bit 0-3: 駒種類（0-14）
 * - bit 4: 成り（0=不成、1=成り）
 * - bit 5: 打ち（0=移動、1=打ち）
 * - bit 6-7: 予約
 */
export const MoveFlags = {
    PIECE_MASK: 0x0f, // 駒種類のマスク
    PROMOTE: 0x10, // 成りフラグ
    DROP: 0x20, // 打ちフラグ
} as const;

/**
 * 駒種類の定数（0-14）
 */
export const PieceTypeBinary = {
    PAWN: 0,
    LANCE: 1,
    KNIGHT: 2,
    SILVER: 3,
    GOLD: 4,
    BISHOP: 5,
    ROOK: 6,
    KING: 7,
    // 成駒
    PROMOTED_PAWN: 8,
    PROMOTED_LANCE: 9,
    PROMOTED_KNIGHT: 10,
    PROMOTED_SILVER: 11,
    PROMOTED_BISHOP: 12,
    PROMOTED_ROOK: 13,
} as const;

/**
 * 定跡エントリーのバイナリ形式
 */
export interface OpeningEntryBinary {
    positionHash: string; // 局面のSFEN（変更なし）
    depth: number; // 深さ（0-255）
    moves: MoveBinary[]; // 候補手の配列
}

/**
 * 盤面座標を0-80の数値に変換
 * @param row 段（1-9）
 * @param column 筋（1-9）
 * @returns 0-80の数値（0は無効/駒打ち用）
 */
export function coordinateToIndex(row: number, column: number): number {
    if (row < 1 || row > 9 || column < 1 || column > 9) {
        return 0;
    }
    return (row - 1) * 9 + (column - 1) + 1;
}

/**
 * 0-80の数値を盤面座標に変換
 * @param index 0-80の数値
 * @returns {row, column} または null
 */
export function indexToCoordinate(index: number): { row: number; column: number } | null {
    if (index <= 0 || index > 81) {
        return null;
    }
    const adjustedIndex = index - 1;
    return {
        row: Math.floor(adjustedIndex / 9) + 1,
        column: (adjustedIndex % 9) + 1,
    };
}

/**
 * MessagePack用のカスタムエンコーダー設定
 */
export const msgpackOptions = {
    // 数値の効率的なエンコード
    forceIntegerToFloat: false,
    // 小さい整数の最適化
    preferMap: false,
    // バイナリデータの効率化
    useBigInt64: false,
};
