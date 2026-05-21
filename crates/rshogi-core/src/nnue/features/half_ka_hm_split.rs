//! HalfKaHmSplit 特徴量
//!
//! Half-Mirror King + SplitPlane（両玉を別 plane）
//!
//! - キングバケット: 45（Half-Mirror）
//! - 入力次元: 76,950 (45×1710)
//!
//! 自玉が動いた場合にアキュムレータの全計算が必要になる。

use super::Feature;
use super::TriggerEvent;
use crate::nnue::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use crate::nnue::bona_piece::BonaPiece;
use crate::nnue::bona_piece_halfka_hm_split::{
    halfka_index, is_hm_mirror, king_bucket, pack_bonapiece,
};
use crate::nnue::constants::HALFKA_HM_SPLIT_DIMENSIONS;
use crate::nnue::piece_list::PieceNumber;
use crate::position::Position;
use crate::types::{Color, Square};

/// HalfKaHmSplit 特徴量
#[allow(non_camel_case_types)]
pub struct HalfKaHmSplit;

impl Feature for HalfKaHmSplit {
    /// 特徴量の次元数: 45×1710 = 76,950
    const DIMENSIONS: usize = HALFKA_HM_SPLIT_DIMENSIONS;

    /// 同時にアクティブになる最大数（合法局面で 40 駒固定）
    const MAX_ACTIVE: usize = 40;

    /// 自玉が動いた場合に全計算
    const REFRESH_TRIGGER: TriggerEvent = TriggerEvent::FriendKingMoved;

    /// アクティブな特徴量インデックスを追記
    #[inline]
    fn append_active_indices(
        pos: &Position,
        perspective: Color,
        active: &mut IndexList<MAX_ACTIVE_FEATURES>,
    ) {
        let king_sq = pos.king_square(perspective);
        let kb = king_bucket(king_sq, perspective);
        let hm_mirror = is_hm_mirror(king_sq, perspective);

        let pieces = if perspective == Color::Black {
            pos.piece_list().piece_list_fb()
        } else {
            pos.piece_list().piece_list_fw()
        };

        for bp in &pieces[..PieceNumber::NB] {
            if *bp != BonaPiece::ZERO {
                let packed = pack_bonapiece(*bp, hm_mirror);
                let _ = active.push(halfka_index(kb, packed));
            }
        }
    }

    /// 変化した特徴量インデックスを追記
    #[inline]
    fn append_changed_indices(
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
        removed: &mut IndexList<MAX_CHANGED_FEATURES>,
        added: &mut IndexList<MAX_CHANGED_FEATURES>,
    ) {
        let kb = king_bucket(king_sq, perspective);
        let hm_mirror = is_hm_mirror(king_sq, perspective);

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
                let packed = pack_bonapiece(old_bp, hm_mirror);
                let _ = removed.push(halfka_index(kb, packed));
            }
            if new_bp != BonaPiece::ZERO {
                let packed = pack_bonapiece(new_bp, hm_mirror);
                let _ = added.push(halfka_index(kb, packed));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dimensions() {
        assert_eq!(HalfKaHmSplit::DIMENSIONS, 76_950);
    }

    #[test]
    fn test_max_active() {
        assert_eq!(HalfKaHmSplit::MAX_ACTIVE, 40);
    }

    #[test]
    fn test_refresh_trigger() {
        assert_eq!(HalfKaHmSplit::REFRESH_TRIGGER, TriggerEvent::FriendKingMoved);
    }

    #[test]
    fn test_append_active_indices_startpos() {
        let mut pos = crate::position::Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        let mut active = IndexList::new();

        HalfKaHmSplit::append_active_indices(&pos, Color::Black, &mut active);

        // 初期局面: 盤上 38 駒 + 両王 2 = 40
        assert_eq!(active.len(), 40);
    }
}
