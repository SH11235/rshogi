//! NNUE定数定義
//!
//! YaneuraOu の HalfKP 256x2-32-32 アーキテクチャに基づき、
//! ネットワーク構造とスケーリングに関する定数をまとめる。

/// 評価関数ファイルのバージョン（YaneuraOu互換）
pub const NNUE_VERSION: u32 = 0x7AF32F16;

/// アーキテクチャ文字列の最大長（破損ファイル/DoS対策）
pub const MAX_ARCH_LEN: usize = 4096;

/// 評価値のスケーリング（水匠5用: 24）
///
/// FV_SCALEは評価関数の訓練時に決まるパラメータ。
/// 同じファイル形式でも評価関数によって異なる場合がある。
/// 例: YaneuraOuのデフォルトは16だが、水匠5は24を使用。
pub const FV_SCALE: i32 = 24;

/// 評価値のスケーリング（デフォルト: 16）
///
/// nnue-pytorchでハードコードされている値（kBiasScale = 600 * 16 = 9600）。
/// YaneuraOuのデフォルト値でもある。
/// bullet-shogiで学習したモデル（scale=600）もこの値で動作する。
pub const FV_SCALE_HALFKA: i32 = 16;

/// 重みのスケーリングビット数
pub const WEIGHT_SCALE_BITS: u32 = 6;

/// SCReLU のデフォルト QA 値
///
/// FT SCReLU 出力は QA に依存せず 0〜127 に正規化されるため、
/// L1/L2 の積和スケールは常に `SCRELU_DEFAULT_QA × QB` になる。
/// QA > 127 の場合は bias スケールを調整する必要がある。
pub const SCRELU_DEFAULT_QA: i32 = 127;

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

// =============================================================================
// HalfKA_hm^ アーキテクチャ用定数
// =============================================================================

/// HalfKA_hm^のバージョン（nnue-pytorch互換）
pub const NNUE_VERSION_HALFKA: u32 = 0x7AF32F20;

/// キングバケット数（Half-Mirror: 9段 × 5筋）
pub const KING_BUCKETS: usize = 45;

/// 駒入力数（DISTINGUISH_GOLDS有効時のe_king = 1629）
pub const PIECE_INPUTS_HALFKA: usize = 1629;

/// HalfKA_hm^のベース入力数（キングバケット × 駒入力）
pub const BASE_INPUTS_HALFKA: usize = KING_BUCKETS * PIECE_INPUTS_HALFKA; // 73,305

/// HalfKA_hm^の総入力次元数
///
/// nnue-pytorch標準のcoalesce済みモデル専用。
/// Factorizationの重みはBase側に畳み込み済みのため、推論時はBaseのみで計算する。
/// これにより特徴量数が半減（80→40）し、NPSが約20%向上する。
///
/// 非coalesceモデル（74,934次元）はサポートしない。
/// nnue-pytorch serialize.py でエクスポートすると自動的にcoalesceされる。
pub const HALFKA_HM_DIMENSIONS: usize = BASE_INPUTS_HALFKA; // 73,305

/// HalfKA_hm^のFactorization込み入力次元数（未coalesce）
///
/// 訓練時のみ使用。推論用モデルは serialize.py で自動的に coalesce される。
/// この定数は互換性エラー検出のために定義。
pub const HALFKA_HM_DIMENSIONS_FACTORIZED: usize = BASE_INPUTS_HALFKA + PIECE_INPUTS_HALFKA; // 74,934

/// 隠れ層1の次元数（YaneuraOu classic）
pub const HIDDEN1_DIMENSIONS: usize = 32;

/// 隠れ層2の次元数（YaneuraOu classic）
pub const HIDDEN2_DIMENSIONS: usize = 32;

/// 出力次元数
pub const OUTPUT_DIMENSIONS: usize = 1;

// =============================================================================
// nnue-pytorch LayerStacks アーキテクチャ用定数
// =============================================================================

/// nnue-pytorch の Feature Transformer 出力次元数（片方の視点）
pub const NNUE_PYTORCH_L1: usize = 1536;

/// nnue-pytorch の L2 次元数
pub const NNUE_PYTORCH_L2: usize = 15;

/// nnue-pytorch の L3 次元数
pub const NNUE_PYTORCH_L3: usize = 32;

/// LayerStacks のバケット数
pub const NUM_LAYER_STACK_BUCKETS: usize = 9;

/// LayerStacks L1層の出力次元数（L2 + 1 = 16）
pub const LAYER_STACK_L1_OUT: usize = NNUE_PYTORCH_L2 + 1; // 16

/// LayerStacks L2層の入力次元数（L2 * 2 = 30）
pub const LAYER_STACK_L2_IN: usize = NNUE_PYTORCH_L2 * 2; // 30

/// nnue-pytorch の評価値スケーリング
pub const NNUE_PYTORCH_NNUE2SCORE: i32 = 600;

/// nnue-pytorch の隠れ層重みスケール
pub const NNUE_PYTORCH_WEIGHT_SCALE_HIDDEN: i32 = 64;

/// nnue-pytorch の出力層重みスケール
pub const NNUE_PYTORCH_WEIGHT_SCALE_OUT: i32 = 16;

/// nnue-pytorch の量子化単位
pub const NNUE_PYTORCH_QUANTIZED_ONE: i32 = 127;

// =============================================================================
// SCReLU (Squared Clipped ReLU) 用定数
// =============================================================================

/// SCReLU 量子化係数 (bullet-shogi 準拠)
///
/// SCReLU では clamp(x, 0, QA)² を計算する。
/// QA = 127 のとき、最大出力は 127² = 16,129。
///
/// スケーリング設計:
/// - 入力: i16 (FeatureTransformer出力、範囲 [-QA, QA])
/// - 出力: i32 (最大 QA² = 16,129)
/// - オーバーフロー検証: 16,129 × 127 × 512 < i32_MAX ✓
pub const SCRELU_QA: i16 = 127;

/// SCReLU L1層以降の量子化係数 (bullet-shogi 準拠)
///
/// L1層以降では QB = 64 を使用。
pub const SCRELU_QB: i16 = 64;

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
