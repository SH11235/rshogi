//! NNUE特徴量モジュール
//!
//! YaneuraOu の FeatureSet/Feature 構造に準拠した特徴量定義。
//! 将来の sfnn 対応を見据えた拡張可能な設計。

mod half_ka;
mod half_ka_hm;
mod half_kp;

pub use half_ka::HalfKA;
pub use half_ka_hm::HalfKA_hm;
pub use half_kp::HalfKP;

use super::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use super::diff::ChangedFeatures;
use crate::position::Position;
use crate::types::{Color, Square};

// =============================================================================
// TriggerEvent - リフレッシュトリガー
// =============================================================================

/// リフレッシュトリガー（YO TriggerEvent 相当）
///
/// アキュムレータの全計算が必要になる条件を定義する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerEvent {
    /// 常に差分計算可能（リフレッシュ不要）
    None,
    /// 自玉が動いた場合に全計算
    FriendKingMoved,
    /// 敵玉が動いた場合に全計算
    EnemyKingMoved,
    /// どちらかの玉が動いた場合に全計算
    AnyKingMoved,
    /// 常に全計算
    AnyPieceMoved,
}

// =============================================================================
// Feature trait - 各特徴量型が実装する基本trait
// =============================================================================

/// Feature trait（YO Feature 相当）
///
/// 各特徴量型が実装する基本trait。
/// 将来 sfnn 対応時に HalfKAv2 などを追加する際も同じインターフェースを使用する。
pub trait Feature {
    /// 特徴量の次元数
    const DIMENSIONS: usize;
    /// 同時にアクティブになる最大数
    const MAX_ACTIVE: usize;
    /// リフレッシュトリガー
    const REFRESH_TRIGGER: TriggerEvent;

    /// アクティブな特徴量インデックスを追記
    fn append_active_indices(
        pos: &Position,
        perspective: Color,
        active: &mut IndexList<MAX_ACTIVE_FEATURES>,
    );

    /// 変化した特徴量インデックスを追記
    fn append_changed_indices(
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
        removed: &mut IndexList<MAX_CHANGED_FEATURES>,
        added: &mut IndexList<MAX_CHANGED_FEATURES>,
    );
}

// =============================================================================
// FeatureSet trait - 複数Featureを合成するインターフェース
// =============================================================================

/// FeatureSet trait（複数Featureを合成するインターフェース）
///
/// 将来 sfnn 対応時に SfnnFeatureSet などを追加する際も同じインターフェースを使用する。
pub trait FeatureSet {
    /// 特徴量の次元数
    const DIMENSIONS: usize;
    /// 同時にアクティブになる最大数
    const MAX_ACTIVE: usize;
    /// リフレッシュトリガー配列（複数Featureの合成時に使用）
    const REFRESH_TRIGGERS: &'static [TriggerEvent];

    /// アクティブな特徴量インデックスを取得
    fn collect_active_indices(pos: &Position, perspective: Color)
        -> IndexList<MAX_ACTIVE_FEATURES>;

    /// 変化した特徴量インデックスを取得
    fn collect_changed_indices(
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
    ) -> ChangedFeatures;

    /// リフレッシュが必要かどうかを判定
    fn needs_refresh(dirty_piece: &DirtyPiece, perspective: Color) -> bool;
}

// =============================================================================
// HalfKPFeatureSet - classic NNUE 用の FeatureSet
// =============================================================================

/// HalfKP 用の FeatureSet（classic NNUE 用）
pub struct HalfKPFeatureSet;

impl FeatureSet for HalfKPFeatureSet {
    const DIMENSIONS: usize = HalfKP::DIMENSIONS;
    const MAX_ACTIVE: usize = HalfKP::MAX_ACTIVE;
    const REFRESH_TRIGGERS: &'static [TriggerEvent] = &[TriggerEvent::FriendKingMoved];

    #[inline]
    fn collect_active_indices(
        pos: &Position,
        perspective: Color,
    ) -> IndexList<MAX_ACTIVE_FEATURES> {
        let mut active = IndexList::new();
        HalfKP::append_active_indices(pos, perspective, &mut active);
        active
    }

    #[inline]
    fn collect_changed_indices(
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
    ) -> ChangedFeatures {
        let mut removed = IndexList::new();
        let mut added = IndexList::new();
        HalfKP::append_changed_indices(dirty_piece, perspective, king_sq, &mut removed, &mut added);
        (removed, added)
    }

    #[inline]
    fn needs_refresh(dirty_piece: &DirtyPiece, perspective: Color) -> bool {
        dirty_piece.king_moved[perspective.index()]
    }
}

// =============================================================================
// HalfKA_hmFeatureSet - HalfKA_hm^ NNUE 用の FeatureSet
// =============================================================================

/// HalfKA_hm^ 用の FeatureSet（nnue-pytorch互換）
///
/// Half-Mirror King + All pieces with Factorization
#[allow(non_camel_case_types)]
pub struct HalfKA_hm_FeatureSet;

impl FeatureSet for HalfKA_hm_FeatureSet {
    const DIMENSIONS: usize = HalfKA_hm::DIMENSIONS;
    const MAX_ACTIVE: usize = HalfKA_hm::MAX_ACTIVE;
    const REFRESH_TRIGGERS: &'static [TriggerEvent] = &[TriggerEvent::FriendKingMoved];

    #[inline]
    fn collect_active_indices(
        pos: &Position,
        perspective: Color,
    ) -> IndexList<MAX_ACTIVE_FEATURES> {
        let mut active = IndexList::new();
        HalfKA_hm::append_active_indices(pos, perspective, &mut active);
        active
    }

    #[inline]
    fn collect_changed_indices(
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
    ) -> ChangedFeatures {
        let mut removed = IndexList::new();
        let mut added = IndexList::new();
        HalfKA_hm::append_changed_indices(
            dirty_piece,
            perspective,
            king_sq,
            &mut removed,
            &mut added,
        );
        (removed, added)
    }

    #[inline]
    fn needs_refresh(dirty_piece: &DirtyPiece, perspective: Color) -> bool {
        dirty_piece.king_moved[perspective.index()]
    }
}

// =============================================================================
// HalfKAFeatureSet - HalfKA NNUE 用の FeatureSet
// =============================================================================

/// HalfKA 用の FeatureSet（non-mirror）
pub struct HalfKAFeatureSet;

impl FeatureSet for HalfKAFeatureSet {
    const DIMENSIONS: usize = HalfKA::DIMENSIONS;
    const MAX_ACTIVE: usize = HalfKA::MAX_ACTIVE;
    const REFRESH_TRIGGERS: &'static [TriggerEvent] = &[TriggerEvent::FriendKingMoved];

    #[inline]
    fn collect_active_indices(
        pos: &Position,
        perspective: Color,
    ) -> IndexList<MAX_ACTIVE_FEATURES> {
        let mut active = IndexList::new();
        HalfKA::append_active_indices(pos, perspective, &mut active);
        active
    }

    #[inline]
    fn collect_changed_indices(
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
    ) -> ChangedFeatures {
        let mut removed = IndexList::new();
        let mut added = IndexList::new();
        HalfKA::append_changed_indices(dirty_piece, perspective, king_sq, &mut removed, &mut added);
        (removed, added)
    }

    #[inline]
    fn needs_refresh(dirty_piece: &DirtyPiece, perspective: Color) -> bool {
        dirty_piece.king_moved[perspective.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_needs_refresh_black_king_moved() {
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.king_moved[Color::Black.index()] = true;

        assert!(HalfKPFeatureSet::needs_refresh(&dirty_piece, Color::Black));
        assert!(!HalfKPFeatureSet::needs_refresh(&dirty_piece, Color::White));
    }

    #[test]
    fn test_needs_refresh_white_king_moved() {
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.king_moved[Color::White.index()] = true;

        assert!(!HalfKPFeatureSet::needs_refresh(&dirty_piece, Color::Black));
        assert!(HalfKPFeatureSet::needs_refresh(&dirty_piece, Color::White));
    }

    #[test]
    fn test_needs_refresh_no_king_moved() {
        let dirty_piece = DirtyPiece::new();

        assert!(!HalfKPFeatureSet::needs_refresh(&dirty_piece, Color::Black));
        assert!(!HalfKPFeatureSet::needs_refresh(&dirty_piece, Color::White));
    }
}
