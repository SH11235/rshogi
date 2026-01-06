//! NNUE 差分更新用のヘルパ
//!
//! `DirtyPiece` に基づいて、HalfKP の active index の増減を計算する。
//! FeatureSet を経由して特徴量の変化を取得する。

use super::accumulator::{DirtyPiece, IndexList, MAX_CHANGED_FEATURES};
use super::features::{FeatureSet, HalfKPFeatureSet};
use crate::position::Position;
use crate::types::Color;

/// 変更された特徴量のペア（removed, added）
pub type ChangedFeatures = (IndexList<MAX_CHANGED_FEATURES>, IndexList<MAX_CHANGED_FEATURES>);

/// 差分更新用: 変化した特徴量を取得
///
/// - 戻り値:
///   - removed: 1→0 になった特徴量（削除）
///   - added:   0→1 になった特徴量（追加）
///
/// 玉が動いた場合や判定ができない場合は（removed, added）とも空を返し、
/// 呼び出し側で全計算にフォールバックする前提とする。
pub fn get_changed_features(
    pos: &Position,
    dirty_piece: &DirtyPiece,
    perspective: Color,
) -> ChangedFeatures {
    if pos.previous_state().is_none() {
        // 前の局面が無い（初期状態など）
        return (IndexList::new(), IndexList::new());
    }

    // リフレッシュが必要な場合は空を返す（全計算にフォールバック）
    if HalfKPFeatureSet::needs_refresh(dirty_piece, perspective) {
        return (IndexList::new(), IndexList::new());
    }

    // 玉のマスを取得（後手視点では反転）
    let raw_king_sq = pos.king_square(perspective);
    let king_sq = if perspective == Color::Black {
        raw_king_sq
    } else {
        raw_king_sq.inverse()
    };
    HalfKPFeatureSet::collect_changed_indices(dirty_piece, perspective, king_sq)
}

/// DirtyPieceから変化した特徴量を計算（コア処理）
///
/// `forward_update_incremental` など、玉移動がないことが確認済みの場合に使用。
///
/// - 戻り値:
///   - removed: 1→0 になった特徴量（削除）
///   - added:   0→1 になった特徴量（追加）
///
/// 玉位置は呼び出し側で指定する。祖先探索で玉移動なしが確認済みの場合、
/// 現局面の玉位置を使用できる。
#[inline]
pub fn get_features_from_dirty_piece(
    dirty_piece: &DirtyPiece,
    perspective: Color,
    king_sq: crate::types::Square,
) -> ChangedFeatures {
    HalfKPFeatureSet::collect_changed_indices(dirty_piece, perspective, king_sq)
}
