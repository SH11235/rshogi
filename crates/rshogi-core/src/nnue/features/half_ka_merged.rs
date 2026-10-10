//! HalfKaMerged 特徴量
//!
//! Non-mirror King + MergedPlane（両玉を 1 plane に畳む）
//!
//! - キングバケット: 81（Direct、ミラーなし）
//! - 入力次元: 131,949 (81×1629)
//!
//! 自玉が動いた場合にアキュムレータの全計算が必要になる。

use super::Feature;
use super::TriggerEvent;
use crate::nnue::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use crate::nnue::bona_piece::BonaPiece;
use crate::nnue::bona_piece_halfka_merged::{halfka_index, king_index, pack_bonapiece};
use crate::nnue::constants::HALFKA_MERGED_DIMENSIONS;
use crate::nnue::piece_list::PieceNumber;
use crate::position::Position;
use crate::types::{Color, Square};

/// HalfKaMerged 特徴量
pub struct HalfKaMerged;

impl Feature for HalfKaMerged {
    /// 特徴量の次元数: 81×1629 = 131,949
    const DIMENSIONS: usize = HALFKA_MERGED_DIMENSIONS;

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
        let k_index = king_index(king_sq, perspective);

        let pieces = if perspective == Color::Black {
            pos.piece_list().piece_list_fb()
        } else {
            pos.piece_list().piece_list_fw()
        };

        active.extend(
            pieces[..PieceNumber::NB]
                .iter()
                .filter(|bp| **bp != BonaPiece::ZERO)
                .map(|bp| halfka_index(k_index, pack_bonapiece(*bp))),
        );
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
        let k_index = king_index(king_sq, perspective);

        removed.extend_pairs(
            added,
            (0..dirty_piece.dirty_num as usize).map(|i| {
                let cp = &dirty_piece.changed_piece[i];
                let (old_bp, new_bp) = if perspective == Color::Black {
                    (cp.old_piece.fb, cp.new_piece.fb)
                } else {
                    (cp.old_piece.fw, cp.new_piece.fw)
                };
                let index = |bp: BonaPiece| {
                    (bp != BonaPiece::ZERO).then(|| halfka_index(k_index, pack_bonapiece(bp)))
                };
                (index(old_bp), index(new_bp))
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dimensions() {
        assert_eq!(HalfKaMerged::DIMENSIONS, 131_949);
    }

    #[test]
    fn test_append_active_indices_startpos() {
        let mut pos = crate::position::Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        let mut active = IndexList::new();

        HalfKaMerged::append_active_indices(&pos, Color::Black, &mut active);

        // 初期局面: 盤上 38 駒 + 両王 2 = 40
        assert_eq!(active.len(), 40);
    }
}
