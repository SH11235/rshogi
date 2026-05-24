//! LayerStack network が依存する FT (Feature Transformer) を抽象化する trait と、
//! それを実装する 5 種類の marker type (`HalfKpSpec` 等)。
//!
//! `FeatureTransformerLayerStacks<L1, FT>` / `NetworkLayerStacks<L1, ..., FT>`
//! を FT-generic 化するために必要な情報を `LsFeatureSpec` に集約する。
//! 設計の根拠は `docs/decisions/2026-05-24-build-edition-flavor-design.md`
//! §「LS FT generic 化」を参照。

use super::bona_piece::{BonaPiece, halfkp_index};
use super::constants::{
    HALFKA_DIMENSIONS, HALFKA_HM_DIMENSIONS, HALFKA_HM_SPLIT_DIMENSIONS, HALFKA_MERGED_DIMENSIONS,
    HALFKP_DIMENSIONS,
};
use super::features::{
    Feature, FeatureSet, HalfKP, HalfKPFeatureSet, HalfKaHmMerged, HalfKaHmMergedFeatureSet,
    HalfKaHmSplit, HalfKaHmSplitFeatureSet, HalfKaMerged, HalfKaMergedFeatureSet, HalfKaSplit,
    HalfKaSplitFeatureSet,
};
use crate::types::{Color, Square};

/// LayerStack の FT (Feature Transformer) を抽象化する trait。
///
/// 5 種類の FT (`HalfKp` / `HalfKaSplit` / `HalfKaMerged` / `HalfKaHmSplit` /
/// `HalfKaHmMerged`) が `LsFeatureSpec` を実装し、`FeatureTransformerLayerStacks`
/// / `NetworkLayerStacks` の type parameter として渡される。
///
/// `Feature` / `FeatureSet` trait と切り離した独立 trait としているのは、
/// `feature_index` だけは LS の cache idx_fn / fast diff path から呼ばれる
/// per-call なホットメソッドで、FT 別 helper module を namespace 統一で参照する
/// 用途に特化しているため。
pub trait LsFeatureSpec: 'static {
    /// 対応する `FeatureSet` 型 (`needs_refresh` / `collect_*_indices` を提供)。
    type Set: FeatureSet;

    /// 対応する `Feature` 型 (`append_active_indices` / `append_changed_indices` を提供)。
    type Feature: Feature;

    /// 特徴量の入力次元 (FT weight 行数)。`Self::Set::DIMENSIONS` と一致する。
    const DIMENSIONS: usize;

    /// 単一 `BonaPiece` を feature index に変換する。
    ///
    /// `try_apply_dirty_piece_fast` (DirtyPiece の old/new BonaPiece → index 変換) と
    /// `refresh_perspective_with_cache` (cache idx_fn) の両方から呼ばれる。
    /// 呼び出し元は `BonaPiece::ZERO` を除外済みを前提とする。
    fn feature_index(bp: BonaPiece, perspective: Color, king_sq: Square) -> usize;
}

/// HalfKP (classic NNUE) 用の LS FT 仕様。
pub struct HalfKpSpec;

impl LsFeatureSpec for HalfKpSpec {
    type Set = HalfKPFeatureSet;
    type Feature = HalfKP;
    const DIMENSIONS: usize = HALFKP_DIMENSIONS;

    #[inline]
    fn feature_index(bp: BonaPiece, perspective: Color, king_sq: Square) -> usize {
        // HalfKP は後手視点で king_sq を上下反転する (HM mirror ではなく Y 反転)。
        // `features/half_kp.rs` の `append_active_indices` / `append_changed_indices`
        // と同じ座標系を再現する必要がある。
        let king = if perspective == Color::Black {
            king_sq
        } else {
            king_sq.inverse()
        };
        halfkp_index(king, bp)
    }
}

/// HalfKaSplit (non-mirror, 両玉別 plane) 用の LS FT 仕様。
pub struct HalfKaSplitSpec;

impl LsFeatureSpec for HalfKaSplitSpec {
    type Set = HalfKaSplitFeatureSet;
    type Feature = HalfKaSplit;
    const DIMENSIONS: usize = HALFKA_DIMENSIONS;

    #[inline]
    fn feature_index(bp: BonaPiece, perspective: Color, king_sq: Square) -> usize {
        use super::bona_piece_halfka_split::{halfka_index, king_index};
        let k_index = king_index(king_sq, perspective);
        halfka_index(k_index, bp.value() as usize)
    }
}

/// HalfKaMerged (non-mirror, 両玉同一 plane) 用の LS FT 仕様。
pub struct HalfKaMergedSpec;

impl LsFeatureSpec for HalfKaMergedSpec {
    type Set = HalfKaMergedFeatureSet;
    type Feature = HalfKaMerged;
    const DIMENSIONS: usize = HALFKA_MERGED_DIMENSIONS;

    #[inline]
    fn feature_index(bp: BonaPiece, perspective: Color, king_sq: Square) -> usize {
        use super::bona_piece_halfka_merged::{halfka_index, king_index, pack_bonapiece};
        let k_index = king_index(king_sq, perspective);
        let packed = pack_bonapiece(bp);
        halfka_index(k_index, packed)
    }
}

/// HalfKaHmSplit (Half-Mirror, 両玉別 plane) 用の LS FT 仕様。
pub struct HalfKaHmSplitSpec;

impl LsFeatureSpec for HalfKaHmSplitSpec {
    type Set = HalfKaHmSplitFeatureSet;
    type Feature = HalfKaHmSplit;
    const DIMENSIONS: usize = HALFKA_HM_SPLIT_DIMENSIONS;

    #[inline]
    fn feature_index(bp: BonaPiece, perspective: Color, king_sq: Square) -> usize {
        use super::bona_piece_halfka_hm_split::{
            halfka_index, is_hm_mirror, king_bucket, pack_bonapiece,
        };
        let kb = king_bucket(king_sq, perspective);
        let hm = is_hm_mirror(king_sq, perspective);
        let packed = pack_bonapiece(bp, hm);
        halfka_index(kb, packed)
    }
}

/// HalfKaHmMerged (Half-Mirror, 両玉同一 plane、現状の v100 系) 用の LS FT 仕様。
pub struct HalfKaHmMergedSpec;

impl LsFeatureSpec for HalfKaHmMergedSpec {
    type Set = HalfKaHmMergedFeatureSet;
    type Feature = HalfKaHmMerged;
    const DIMENSIONS: usize = HALFKA_HM_DIMENSIONS;

    #[inline]
    fn feature_index(bp: BonaPiece, perspective: Color, king_sq: Square) -> usize {
        use super::bona_piece_halfka_hm_merged::{
            halfka_index, is_hm_mirror, king_bucket, pack_bonapiece,
        };
        let kb = king_bucket(king_sq, perspective);
        let hm = is_hm_mirror(king_sq, perspective);
        let packed = pack_bonapiece(bp, hm);
        halfka_index(kb, packed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dimensions_match_feature_set() {
        assert_eq!(HalfKpSpec::DIMENSIONS, <HalfKPFeatureSet as FeatureSet>::DIMENSIONS);
        assert_eq!(HalfKaSplitSpec::DIMENSIONS, <HalfKaSplitFeatureSet as FeatureSet>::DIMENSIONS);
        assert_eq!(
            HalfKaMergedSpec::DIMENSIONS,
            <HalfKaMergedFeatureSet as FeatureSet>::DIMENSIONS
        );
        assert_eq!(
            HalfKaHmSplitSpec::DIMENSIONS,
            <HalfKaHmSplitFeatureSet as FeatureSet>::DIMENSIONS
        );
        assert_eq!(
            HalfKaHmMergedSpec::DIMENSIONS,
            <HalfKaHmMergedFeatureSet as FeatureSet>::DIMENSIONS
        );
    }
}
