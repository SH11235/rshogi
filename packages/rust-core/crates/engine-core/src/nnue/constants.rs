//! NNUE定数定義
//!
//! YaneuraOu の HalfKP 256x2-32-32 アーキテクチャに基づき、
//! ネットワーク構造とスケーリングに関する定数をまとめる。

/// 評価関数ファイルのバージョン（YaneuraOu互換）
pub const NNUE_VERSION: u32 = 0x7AF32F16;

/// 評価値のスケーリング（デフォルト: 16）
pub const FV_SCALE: i32 = 16;

/// 重みのスケーリングビット数
pub const WEIGHT_SCALE_BITS: u32 = 6;

/// キャッシュラインサイズ（バイト）
pub const CACHE_LINE_SIZE: usize = 64;

/// SIMD幅（バイト）
#[cfg(target_feature = "avx2")]
pub const SIMD_WIDTH: usize = 32;

#[cfg(all(target_feature = "sse2", not(target_feature = "avx2")))]
pub const SIMD_WIDTH: usize = 16;

#[cfg(not(any(target_feature = "avx2", target_feature = "sse2")))]
pub const SIMD_WIDTH: usize = 8;

/// 変換後の次元数（片方の視点）
pub const TRANSFORMED_FEATURE_DIMENSIONS: usize = 256;

/// リフレッシュトリガーの数（YO kRefreshTriggers.size() 相当）
/// HalfKP の場合は FriendKingMoved のみで 1
///
/// 注意: この値を変更する場合、以下の箇所も更新が必要:
/// - `Accumulator` の `accumulation` 配列サイズ
/// - `FeatureTransformer` の `refresh_accumulator` / `update_accumulator` / `transform`
/// - `HalfKPFeatureSet::REFRESH_TRIGGERS`
pub const NUM_REFRESH_TRIGGERS: usize = 1;

/// HalfKP特徴量の次元数
/// 81（玉の位置）× FE_END（BonaPiece数）
pub const HALFKP_DIMENSIONS: usize = 81 * super::bona_piece::FE_END;

/// 隠れ層1の次元数
pub const HIDDEN1_DIMENSIONS: usize = 32;

/// 隠れ層2の次元数
pub const HIDDEN2_DIMENSIONS: usize = 32;

/// 出力次元数
pub const OUTPUT_DIMENSIONS: usize = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(TRANSFORMED_FEATURE_DIMENSIONS, 256);
        assert_eq!(HIDDEN1_DIMENSIONS, 32);
        assert_eq!(HIDDEN2_DIMENSIONS, 32);
        assert_eq!(OUTPUT_DIMENSIONS, 1);
    }
}
