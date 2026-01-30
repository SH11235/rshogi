//! LayerStack NNUE 定数定義
//!
//! 次元定数とスケーリング定数。

// =============================================================================
// 次元定義
// =============================================================================

/// HalfKA 特徴量のマス数
pub const NUM_SQ: usize = 81;

/// HalfKA_hm 入力平面数（905）
///
/// HalfKA_hm特徴量セット:
/// - 駒種(10) × 位置(81) = 810
/// - 自玉位置: 81
/// - 持ち駒: 14
/// - 合計: 810 + 81 + 14 = 905
pub const NUM_PLANES: usize = 905;

/// HalfKA_hm 特徴量の総入力次元数（73,305 per king）
///
/// Training側 bullet-shogi と一致
pub const HALFKA_FEATURES: usize = NUM_PLANES * NUM_SQ; // 905 * 81 = 73,305

/// Feature Transformer 出力次元（片視点）
///
/// "1536-15-64" の 1536
pub const FT_PER_PERSPECTIVE: usize = 1536;

/// Perspective 結合後の次元（STM + NSTM）
pub const PERSPECTIVE_CAT: usize = FT_PER_PERSPECTIVE * 2; // 3072

/// Product Pooling 出力次元（2-to-1 reduction: 3072 → 1536）
pub const PP_OUT: usize = FT_PER_PERSPECTIVE; // 1536

// =============================================================================
// LayerStacks 次元
// =============================================================================

/// L1 層入力次元
pub const L1_IN: usize = PP_OUT; // 1536

/// L1 層出力次元（main(15) + bypass(1)）
pub const L1_OUT: usize = 16;

/// L1 main 出力次元（Dual Activation への入力）
pub const L1_MAIN: usize = 15;

/// Dual Activation 出力次元（SqrCReLU(15) + CReLU(15)）
pub const DUAL_ACT_OUT: usize = 30;

/// L2 層出力次元（"1536-15-64" の 64）
pub const L2_OUT: usize = 64;

// =============================================================================
// スケーリング定数
// =============================================================================

/// 量子化の基準値（127 = 2^7 - 1）
pub const QUANTIZED_ONE: i16 = 127;

/// 隠れ層の重みスケール（2^6 = 64）
pub const WEIGHT_SCALE_HIDDEN: i32 = 64;

/// 出力層の重みスケール（2^4 = 16）
pub const WEIGHT_SCALE_OUT: i32 = 16;

/// NNUE 評価値から cp への変換スケール
pub const NNUE2SCORE: i32 = 600;

// =============================================================================
// バケット数
// =============================================================================

/// 2x2 バケット数
pub const BUCKETS_2X2: usize = 4;

/// 3x3 バケット数
pub const BUCKETS_3X3: usize = 9;

// =============================================================================
// Product Pooling 定数
// =============================================================================

/// Product Pooling の右シフト量
///
/// `/127` を `/128` で近似するため、`>> 7` を使用
pub const PP_SHIFT: u32 = 7;

/// Product Pooling の出力最大値
///
/// `(127 * 127) >> 7 = 126.0039...` → 126 にクランプ
pub const PP_MAX_OUT: u8 = 126;

// =============================================================================
// Dual Activation 定数
// =============================================================================

/// SqrCReLU の右シフト量
///
/// x² >> 19 で [0, 127] に正規化
pub const SQRCRELU_SHIFT: u32 = 19;

/// CReLU の右シフト量
///
/// x >> 6 で [0, 127] に正規化
pub const CRELU_SHIFT: u32 = 6;

/// 活性化関数の出力最大値
pub const ACT_MAX_OUT: i32 = 127;

// =============================================================================
// L2 層定数
// =============================================================================

/// L2 層の右シフト量
pub const L2_SHIFT: u32 = 6;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dimensions() {
        // HalfKA_hm 特徴量次元（training側と一致）
        assert_eq!(HALFKA_FEATURES, 73_305);
        assert_eq!(FT_PER_PERSPECTIVE, 1536);
        assert_eq!(PERSPECTIVE_CAT, 3072);
        assert_eq!(PP_OUT, 1536);
        assert_eq!(L1_OUT, 16);
        assert_eq!(L1_MAIN, 15);
        assert_eq!(DUAL_ACT_OUT, 30);
        assert_eq!(L2_OUT, 64);
    }

    #[test]
    fn test_scaling_constants() {
        assert_eq!(QUANTIZED_ONE, 127);
        assert_eq!(WEIGHT_SCALE_HIDDEN, 64);
        assert_eq!(WEIGHT_SCALE_OUT, 16);
        assert_eq!(NNUE2SCORE, 600);
    }

    #[test]
    fn test_product_pooling_constants() {
        // (127 * 127) >> 7 = 16129 >> 7 = 126.007... → 126
        let max_product = 127u16 * 127;
        let shifted = (max_product >> PP_SHIFT) as u8;
        assert!(shifted <= PP_MAX_OUT);
    }
}
