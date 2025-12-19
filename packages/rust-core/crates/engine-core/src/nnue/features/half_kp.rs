//! HalfKP 特徴量
//!
//! 自玉位置×駒配置（BonaPiece）の組み合わせで特徴量を表現する。
//! YaneuraOu の HalfKP<Friend> に相当する。

use super::{Feature, TriggerEvent};
use crate::nnue::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use crate::nnue::bona_piece::{halfkp_index, BonaPiece, FE_END};
use crate::position::Position;
use crate::types::{Color, PieceType, Square};

/// HalfKP<Friend> 特徴量
///
/// 自玉位置×駒配置（BonaPiece）の組み合わせで特徴量を表現する。
/// 自玉が動いた場合にアキュムレータの全計算が必要になる。
pub struct HalfKP;

impl Feature for HalfKP {
    /// 特徴量の次元数: 81（玉の位置）× FE_END（BonaPiece数）
    const DIMENSIONS: usize = 81 * FE_END;

    /// 同時にアクティブになる最大数: 盤上38駒（玉除く）+ 手駒14 = 52
    const MAX_ACTIVE: usize = 52;

    /// 自玉が動いた場合に全計算
    const REFRESH_TRIGGER: TriggerEvent = TriggerEvent::FriendKingMoved;

    /// アクティブな特徴量インデックスを追記
    ///
    /// 盤上駒および手駒を HalfKP 特徴量に写像する。
    #[inline]
    fn append_active_indices(
        pos: &Position,
        perspective: Color,
        active: &mut IndexList<MAX_ACTIVE_FEATURES>,
    ) {
        let king_sq = pos.king_square(perspective);

        // 盤上の駒
        for sq in pos.occupied().iter() {
            let pc = pos.piece_on(sq);
            if pc.is_none() {
                continue;
            }
            // 玉は特徴量に含めない
            if pc.piece_type() == PieceType::King {
                continue;
            }

            let bp = BonaPiece::from_piece_square(pc, sq, perspective);
            if bp != BonaPiece::ZERO {
                let index = halfkp_index(king_sq, bp);
                active.push(index);
            }
        }

        // 手駒の特徴量
        for owner in [Color::Black, Color::White] {
            for pt in [
                PieceType::Pawn,
                PieceType::Lance,
                PieceType::Knight,
                PieceType::Silver,
                PieceType::Gold,
                PieceType::Bishop,
                PieceType::Rook,
            ] {
                let count = pos.hand(owner).count(pt) as u8;
                if count == 0 {
                    continue;
                }
                let bp = BonaPiece::from_hand_piece(perspective, owner, pt, count);
                if bp != BonaPiece::ZERO {
                    let index = halfkp_index(king_sq, bp);
                    active.push(index);
                }
            }
        }
    }

    /// 変化した特徴量インデックスを追記
    ///
    /// DirtyPieceから変化した特徴量を計算する。
    #[inline]
    fn append_changed_indices(
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
        removed: &mut IndexList<MAX_CHANGED_FEATURES>,
        added: &mut IndexList<MAX_CHANGED_FEATURES>,
    ) {
        // 盤上駒の変化を処理
        for dp in dirty_piece.pieces() {
            // 盤上から消える側（old）
            if !dp.old_piece.is_none() {
                if let Some(sq) = dp.old_sq {
                    let bp = BonaPiece::from_piece_square(dp.old_piece, sq, perspective);
                    if bp != BonaPiece::ZERO {
                        removed.push(halfkp_index(king_sq, bp));
                    }
                }
            }

            // 盤上に現れる側（new）
            if !dp.new_piece.is_none() {
                if let Some(sq) = dp.new_sq {
                    let bp = BonaPiece::from_piece_square(dp.new_piece, sq, perspective);
                    if bp != BonaPiece::ZERO {
                        added.push(halfkp_index(king_sq, bp));
                    }
                }
            }
        }

        // 手駒の変化を反映
        for hc in dirty_piece.hand_changes() {
            // やねうら王同様、手駒は種類×枚数の組み合わせで 1 つの BonaPiece を表現する。
            if hc.old_count != hc.new_count {
                // 旧カウント分の特徴量を削除
                if hc.old_count > 0 {
                    let bp_old = BonaPiece::from_hand_piece(
                        perspective,
                        hc.owner,
                        hc.piece_type,
                        hc.old_count,
                    );
                    if bp_old != BonaPiece::ZERO {
                        removed.push(halfkp_index(king_sq, bp_old));
                    }
                }
                // 新カウント分の特徴量を追加
                if hc.new_count > 0 {
                    let bp_new = BonaPiece::from_hand_piece(
                        perspective,
                        hc.owner,
                        hc.piece_type,
                        hc.new_count,
                    );
                    if bp_new != BonaPiece::ZERO {
                        added.push(halfkp_index(king_sq, bp_new));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::Position;

    #[test]
    fn test_halfkp_dimensions() {
        // HALFKP_DIMENSIONS と一致することを確認
        assert_eq!(HalfKP::DIMENSIONS, 81 * FE_END);
    }

    #[test]
    fn test_halfkp_max_active() {
        // MAX_ACTIVE_FEATURES と一致することを確認
        assert_eq!(HalfKP::MAX_ACTIVE, 52);
    }

    #[test]
    fn test_halfkp_refresh_trigger() {
        assert_eq!(HalfKP::REFRESH_TRIGGER, TriggerEvent::FriendKingMoved);
    }

    #[test]
    fn test_append_active_indices_startpos() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        let mut active = IndexList::new();

        HalfKP::append_active_indices(&pos, Color::Black, &mut active);

        // 初期局面: 盤上38駒 + 手駒0 = 38
        assert_eq!(active.len(), 38);
    }
}
