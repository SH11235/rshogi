//! HalfKA 特徴量
//!
//! King + All pieces (non-mirror)
//!
//! 主な特徴:
//! - キング位置: 81マス直指定（ミラーなし）
//! - 入力次元: 138,510 (81×1710)
//!
//! 注意: nnue-pytorchのcoalesce済みモデル専用。
//! Factorizationの重みはBase側に畳み込み済みのため、推論時はBaseのみで計算する。

use super::Feature;
use super::TriggerEvent;
use crate::nnue::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use crate::nnue::bona_piece::BonaPiece;
use crate::nnue::bona_piece_halfka::{halfka_index, king_index};
use crate::nnue::constants::HALFKA_DIMENSIONS;
use crate::nnue::piece_list::PieceNumber;
use crate::position::Position;
use crate::types::{Color, Square};

/// HalfKA 特徴量
///
/// キング位置は81マス直指定（ミラーなし）。
/// 自玉が動いた場合にアキュムレータの全計算が必要になる。
pub struct HalfKA;

impl Feature for HalfKA {
    /// 特徴量の次元数: 81×1710 = 138,510
    const DIMENSIONS: usize = HALFKA_DIMENSIONS;

    /// 同時にアクティブになる最大数（合法局面での理論上限）
    ///
    /// 将棋の合法局面では駒の総数は40個固定:
    /// - 盤上駒（王含む）+ 手駒 = 40
    ///
    /// coalesce済みモデルでは各駒が1特徴量なので MAX_ACTIVE = 40。
    ///
    /// 注意: この値は理論上限。実際のIndexListは`MAX_ACTIVE_FEATURES = 54`を使用し、
    /// テスト用の非合法局面（駒数超過）にも対応できる安全マージンを持つ。
    const MAX_ACTIVE: usize = 40;

    /// 自玉が動いた場合に全計算
    const REFRESH_TRIGGER: TriggerEvent = TriggerEvent::FriendKingMoved;

    /// アクティブな特徴量インデックスを追記
    ///
    /// PieceList の全40エントリを走査（玉含む）。
    #[inline]
    fn append_active_indices(
        pos: &Position,
        perspective: Color,
        active: &mut IndexList<MAX_ACTIVE_FEATURES>,
    ) {
        let king_sq = pos.king_square(perspective);
        let k_index = king_index(king_sq, perspective);

        let pieces = if perspective == Color::Black {
            pos.piece_list().piece_list_fb()
        } else {
            pos.piece_list().piece_list_fw()
        };

        for bp in &pieces[..PieceNumber::NB] {
            if *bp != BonaPiece::ZERO {
                let _ = active.push(halfka_index(k_index, bp.value() as usize));
            }
        }
    }

    /// 変化した特徴量インデックスを追記
    ///
    /// DirtyPiece の ExtBonaPiece を直接使用。
    #[inline]
    fn append_changed_indices(
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
        removed: &mut IndexList<MAX_CHANGED_FEATURES>,
        added: &mut IndexList<MAX_CHANGED_FEATURES>,
    ) {
        let k_index = king_index(king_sq, perspective);

        for i in 0..dirty_piece.dirty_num as usize {
            let cp = &dirty_piece.changed_piece[i];
            let old_bp = if perspective == Color::Black {
                cp.old_piece.fb
            } else {
                cp.old_piece.fw
            };
            let new_bp = if perspective == Color::Black {
                cp.new_piece.fb
            } else {
                cp.new_piece.fw
            };

            if old_bp != BonaPiece::ZERO {
                let _ = removed.push(halfka_index(k_index, old_bp.value() as usize));
            }
            if new_bp != BonaPiece::ZERO {
                let _ = added.push(halfka_index(k_index, new_bp.value() as usize));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_halfka_dimensions() {
        assert_eq!(HalfKA::DIMENSIONS, 138_510);
    }

    #[test]
    fn test_halfka_max_active() {
        assert_eq!(HalfKA::MAX_ACTIVE, 40);
    }

    #[test]
    fn test_halfka_refresh_trigger() {
        assert_eq!(HalfKA::REFRESH_TRIGGER, TriggerEvent::FriendKingMoved);
    }
}
