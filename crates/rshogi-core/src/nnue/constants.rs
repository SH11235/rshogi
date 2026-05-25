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
// HalfKaHmMerged^ アーキテクチャ用定数
// =============================================================================

/// HalfKaHmMerged^のバージョン（nnue-pytorch互換）
///
/// LayerStack の **legacy layout** (bucket 数固定 9、`num_buckets` field 無し) でも
/// 同じ値を共有する。HalfKa 系 / HalfKaHm 系の `.bin` および legacy LayerStack `.bin`
/// は `arch_str` から特定される。
pub const NNUE_VERSION_HALFKA: u32 = 0x7AF32F20;

/// LayerStack 可変 bucket 数 layout の version (tatara ADR `2026-05-23` 由来)。
///
/// `arch_str` の直後に `num_buckets: u32` field を持つ self-describing layout。
/// 旧版 (`NNUE_VERSION_HALFKA = 0x7AF32F20`) は本 field を持たず、暗黙の
/// `num_buckets = 9` として読む。
pub const NNUE_VERSION_LAYERSTACK_V2: u32 = 0x7AF32F21;

/// キングバケット数（Half-Mirror: 9段 × 5筋）
pub const KING_BUCKETS: usize = 45;

/// 駒入力数（DISTINGUISH_GOLDS有効時のe_king = 1629）
pub const PIECE_INPUTS_HALFKA: usize = 1629;

/// HalfKaHmMerged^のベース入力数（キングバケット × 駒入力）
pub const BASE_INPUTS_HALFKA: usize = KING_BUCKETS * PIECE_INPUTS_HALFKA; // 73,305

/// HalfKaHmMerged^の総入力次元数
///
/// nnue-pytorch標準のcoalesce済みモデル専用。
/// Factorizationの重みはBase側に畳み込み済みのため、推論時はBaseのみで計算する。
/// これにより特徴量数が半減（80→40）し、NPSが約20%向上する。
///
/// 非coalesceモデル（74,934次元）はサポートしない。
/// nnue-pytorch serialize.py でエクスポートすると自動的にcoalesceされる。
pub const HALFKA_HM_DIMENSIONS: usize = BASE_INPUTS_HALFKA; // 73,305

/// HalfKaHmMerged^のFactorization込み入力次元数（未coalesce）
///
/// 訓練時のみ使用。推論用モデルは serialize.py で自動的に coalesce される。
/// この定数は互換性エラー検出のために定義。
pub const HALFKA_HM_DIMENSIONS_FACTORIZED: usize = BASE_INPUTS_HALFKA + PIECE_INPUTS_HALFKA; // 74,934

// =============================================================================
// HalfKaSplit（非ミラー）アーキテクチャ用定数（Hisui 仕様）
// =============================================================================

/// HalfKaSplit（非ミラー）の入力平面数
///
/// Hisui の学習設定: 1548 + 81 * 2 = 1710
pub const HALFKA_PLANES: usize = 1548 + 81 * 2;

/// HalfKaSplit（非ミラー）の総入力次元数
///
/// 81（玉位置）× 1710（入力平面）
pub const HALFKA_DIMENSIONS: usize = HALFKA_PLANES * 81; // 138,510

// =============================================================================
// HalfKaMerged アーキテクチャ用定数（Non-mirror + MergedPlane）
// =============================================================================

/// HalfKaMerged の総入力次元数
///
/// 81（玉位置、Direct）× 1629（両玉を 1 plane に畳んだ入力数）
pub const HALFKA_MERGED_DIMENSIONS: usize = 81 * 1629; // 131,949

// =============================================================================
// HalfKaHmSplit アーキテクチャ用定数（Half-Mirror + SplitPlane）
// =============================================================================

/// HalfKaHmSplit の総入力次元数
///
/// 45（玉位置、Half-Mirror）× 1710（両玉別 plane の入力数）
pub const HALFKA_HM_SPLIT_DIMENSIONS: usize = 45 * 1710; // 76,950

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

/// LayerStacks 16x32 バリアントの L2 直前 main 次元数
pub const LAYER_STACK_16X32_MAIN_DIM: usize = 15;

/// LayerStacks の L2 出力次元数
pub const NNUE_PYTORCH_L3: usize = 32;

/// LayerStacks の **既定** バケット数
///
/// tatara ADR `2026-05-23-num-buckets-configurable.md` §8 の既定 `--num-buckets = 9`
/// と一致。legacy `.bin` (`NNUE_VERSION_HALFKA`、`num_buckets` field 無し) は本値で
/// 読み込む。新 layout (`NNUE_VERSION_LAYERSTACK_V2`) では `.bin` header の
/// `num_buckets: u32` field を読んで上書きする。
pub const DEFAULT_NUM_BUCKETS: usize = 9;

/// LayerStacks の bucket 数の **上限**
///
/// engine 内の `psqt_accumulation` 等の固定長配列のサイズを決める値。`.bin` から
/// 読んだ `num_buckets` が本値を超えると `InvalidData` で reject する。
///
/// 16 は tatara 側の N sweep (例: 5, 8, 9, 12, 16) を吸収しつつ、Accumulator の
/// memory footprint 増分を最小に保つ値として選択 (ADR `2026-05-26` §2.3.2 / §2.6)。
/// 将来 N > 16 を扱いたい場合は本値を上げる。
pub const MAX_LAYER_STACK_BUCKETS: usize = 16;

// NOTE: 旧 `NUM_LAYER_STACK_BUCKETS` は廃止。用途別に以下を使い分ける:
// - 配布 net (legacy `.bin`) の暗黙 bucket 数: `DEFAULT_NUM_BUCKETS`
// - hot-path 固定長配列のサイズ: `MAX_LAYER_STACK_BUCKETS`
// - net instance の実 bucket 数: `NetworkLayerStacks::num_buckets`

/// LayerStacks 16x32 バリアントの L1層出力次元数（main 15 + skip 1 = 16）
pub const LAYER_STACK_16X32_L1_OUT: usize = LAYER_STACK_16X32_MAIN_DIM + 1; // 16

/// LayerStacks 16x32 バリアントの L2層入力次元数（sqr 15 + crelu 15 = 30）
pub const LAYER_STACK_16X32_L2_IN: usize = LAYER_STACK_16X32_MAIN_DIM * 2; // 30

/// LayerStacks 32x32 バリアントの L1層出力次元数（main 31 + skip 1 = 32）
pub const LAYER_STACK_32X32_L1_OUT: usize = 32;

/// LayerStacks 32x32 バリアントの main 次元数
pub const LAYER_STACK_32X32_MAIN_DIM: usize = LAYER_STACK_32X32_L1_OUT - 1; // 31

/// LayerStacks 32x32 バリアントの L2層入力次元数（sqr 31 + crelu 31 = 62）
pub const LAYER_STACK_32X32_L2_IN: usize = LAYER_STACK_32X32_MAIN_DIM * 2; // 62

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
