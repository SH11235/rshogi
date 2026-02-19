//! HalfKP 特徴量
//!
//! 自玉位置×駒配置（BonaPiece）の組み合わせで特徴量を表現する。
//! YaneuraOu の HalfKP<Friend> に相当する。

use super::Feature;
use super::TriggerEvent;
use crate::nnue::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use crate::nnue::bona_piece::{halfkp_index, BonaPiece, FE_END};
use crate::nnue::piece_list::PieceNumber;
use crate::position::Position;
use crate::types::{Color, Square};

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
    /// PieceList を1回走査して全特徴量を生成する（玉除外）。
    #[inline]
    fn append_active_indices(
        pos: &Position,
        perspective: Color,
        active: &mut IndexList<MAX_ACTIVE_FEATURES>,
    ) {
        let raw_king_sq = pos.king_square(perspective);
        let king_sq = if perspective == Color::Black {
            raw_king_sq
        } else {
            raw_king_sq.inverse()
        };

        // PieceList の 0..KING 番目を走査（玉除外）
        let pieces = if perspective == Color::Black {
            pos.piece_list().piece_list_fb()
        } else {
            pos.piece_list().piece_list_fw()
        };

        for bp in &pieces[..PieceNumber::KING as usize] {
            if *bp != BonaPiece::ZERO {
                let _ = active.push(halfkp_index(king_sq, *bp));
            }
        }
    }

    /// 変化した特徴量インデックスを追記
    ///
    /// DirtyPiece の ExtBonaPiece を直接使用する。
    /// HalfKP では King の BonaPiece は FE_END 以上なので自動的にフィルタされる。
    #[inline]
    fn append_changed_indices(
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
        removed: &mut IndexList<MAX_CHANGED_FEATURES>,
        added: &mut IndexList<MAX_CHANGED_FEATURES>,
    ) {
        let king_sq = if perspective == Color::Black {
            king_sq
        } else {
            king_sq.inverse()
        };

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

            // HalfKP: King の BonaPiece (>= FE_END) は除外
            if old_bp != BonaPiece::ZERO && (old_bp.value() as usize) < FE_END {
                let _ = removed.push(halfkp_index(king_sq, old_bp));
            }
            if new_bp != BonaPiece::ZERO && (new_bp.value() as usize) < FE_END {
                let _ = added.push(halfkp_index(king_sq, new_bp));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::accumulator::{ChangedBonaPiece, DirtyPiece};
    use crate::nnue::bona_piece::ExtBonaPiece;
    use crate::nnue::piece_list::PieceNumber;
    use crate::position::Position;
    use crate::types::{Color, File, Piece, PieceType, Rank, Square};

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

    #[test]
    fn test_feature_indices_in_range() {
        // 初期局面の特徴インデックスが範囲内であることを確認
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();

        for perspective in [Color::Black, Color::White] {
            let mut active = IndexList::new();
            HalfKP::append_active_indices(&pos, perspective, &mut active);

            let max_valid_index = 81 * FE_END - 1;
            for (i, &index) in active.iter().enumerate() {
                assert!(
                    index <= max_valid_index,
                    "Feature index {} at position {} exceeds max {} for perspective {:?}",
                    index,
                    i,
                    max_valid_index,
                    perspective
                );
            }
        }
    }

    #[test]
    fn test_bona_piece_values() {
        // BonaPieceの値がやねうら王の定義と一致することを確認
        use crate::nnue::bona_piece::{E_DRAGON, E_GOLD, E_PAWN, F_DRAGON, F_GOLD, F_PAWN};

        // 盤上駒のベース値
        assert_eq!(F_PAWN, 90, "f_pawn should be 90");
        assert_eq!(E_PAWN, 171, "e_pawn should be 171");
        assert_eq!(F_GOLD, 738, "f_gold should be 738");
        assert_eq!(E_GOLD, 819, "e_gold should be 819");
        assert_eq!(F_DRAGON, 1386, "f_dragon should be 1386");
        assert_eq!(E_DRAGON, 1467, "e_dragon should be 1467");

        // FE_END
        assert_eq!(FE_END, 1548, "fe_end should be 1548");
    }

    // =================================================================
    // append_changed_indices のテスト
    // =================================================================

    #[test]
    fn test_append_changed_indices_piece_move() {
        // 駒移動（盤上→盤上）: 7七の歩を7六へ移動
        let sq_77 = Square::new(File::File7, Rank::Rank7);
        let sq_76 = Square::new(File::File7, Rank::Rank6);
        let king_sq = Square::new(File::File5, Rank::Rank9); // 5九

        let old_bp = ExtBonaPiece::from_board(Piece::B_PAWN, sq_77);
        let new_bp = ExtBonaPiece::from_board(Piece::B_PAWN, sq_76);
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: old_bp,
            new_piece: new_bp,
        };

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKP::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 1つの駒が移動: removed=1, added=1
        assert_eq!(removed.len(), 1);
        assert_eq!(added.len(), 1);

        // removed と added のインデックスは異なるはず
        let removed_idx: Vec<_> = removed.iter().copied().collect();
        let added_idx: Vec<_> = added.iter().copied().collect();
        assert_ne!(removed_idx[0], added_idx[0]);
    }

    #[test]
    fn test_append_changed_indices_capture() {
        // 駒取り: 攻め駒が敵駒を取る
        // 例: 2四の歩（先手）が2三に進んで2三の歩（後手）を取る
        let sq_24 = Square::new(File::File2, Rank::Rank4);
        let sq_23 = Square::new(File::File2, Rank::Rank3);
        let king_sq = Square::new(File::File5, Rank::Rank9); // 5九

        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 2;

        // [0]: 動いた駒（先手の歩: 2四→2三）
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(Piece::B_PAWN, sq_24),
            new_piece: ExtBonaPiece::from_board(Piece::B_PAWN, sq_23),
        };

        // [1]: 取られた駒（後手の歩: 盤上2三→先手の手駒へ）
        dirty_piece.piece_no[1] = PieceNumber(1);
        dirty_piece.changed_piece[1] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(Piece::W_PAWN, sq_23),
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
        };

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKP::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 新API: 各 changed_piece エントリが old/new ペアを持つ
        // [0]: 移動駒 old(2四)→new(2三) → removed=1, added=1
        // [1]: 捕獲駒 old(盤上2三)→new(手駒) → removed=1, added=1（手駒BonaPiece < FE_END）
        assert_eq!(removed.len(), 2);
        assert_eq!(added.len(), 2);
    }

    #[test]
    fn test_append_changed_indices_drop() {
        // 打ち込み: 手駒から盤上へ（歩を5五に打つ、手駒は1枚持っていた）
        let king_sq = Square::new(File::File5, Rank::Rank9); // 5九

        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
            new_piece: ExtBonaPiece::from_board(Piece::B_PAWN, Square::SQ_55),
        };

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKP::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 打ち込み: removed=1（手駒BonaPiece）, added=1（盤上BonaPiece）
        assert_eq!(removed.len(), 1);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn test_append_changed_indices_hand_change() {
        // 手駒変化: 歩を1枚取得（0→1）
        // 新APIでは手駒変化もChangedBonaPieceで表現する
        let king_sq = Square::new(File::File5, Rank::Rank9); // 5九

        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::ZERO, // 手駒0枚→ZEROとして表現
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
        };

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKP::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 手駒変化: removed=0（old=ZERO→フィルタ）, added=1（new=手駒1枚目BonaPiece）
        assert_eq!(removed.len(), 0);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn test_append_changed_indices_hand_change_increment() {
        // 手駒変化: 既存の手駒が増える（1→2）
        // 新APIではold/new ExtBonaPieceペアとして表現
        let king_sq = Square::new(File::File5, Rank::Rank9); // 5九

        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 2),
        };

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKP::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 手駒が1→2: removed=1（1枚目のBonaPiece）, added=1（2枚目のBonaPiece）
        assert_eq!(removed.len(), 1);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn test_debug_feature_indices() {
        use crate::nnue::accumulator::{IndexList, MAX_ACTIVE_FEATURES};
        use crate::nnue::bona_piece::{E_PAWN, F_PAWN};

        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();

        // 先手玉と後手玉の位置
        let king_sq_b = pos.king_square(Color::Black);
        let king_sq_w = pos.king_square(Color::White);
        let king_sq_w_inv = king_sq_w.inverse();

        eprintln!("Black King: {:?} (index={})", king_sq_b, king_sq_b.index());
        eprintln!(
            "White King: {:?} (index={}), inverted: {:?} (index={})",
            king_sq_w,
            king_sq_w.index(),
            king_sq_w_inv,
            king_sq_w_inv.index()
        );

        // 7七の歩（先手）のBonaPiece
        let sq_77 = Square::new(File::File7, Rank::Rank7);
        let bp_77_black = crate::nnue::bona_piece::BonaPiece::from_piece_square(
            Piece::B_PAWN,
            sq_77,
            Color::Black,
        );
        let bp_77_white = crate::nnue::bona_piece::BonaPiece::from_piece_square(
            Piece::B_PAWN,
            sq_77,
            Color::White,
        );
        eprintln!(
            "7七先手歩: sq_index={}, Black view BP={}, White view BP={}",
            sq_77.index(),
            bp_77_black.value(),
            bp_77_white.value()
        );
        eprintln!(
            "  Expected Black: F_PAWN({}) + {} = {}",
            F_PAWN,
            sq_77.index(),
            F_PAWN as usize + sq_77.index()
        );
        eprintln!(
            "  Expected White: E_PAWN({}) + {} = {}",
            E_PAWN,
            sq_77.inverse().index(),
            E_PAWN as usize + sq_77.inverse().index()
        );

        // 先手視点の特徴量
        let mut active_b: IndexList<MAX_ACTIVE_FEATURES> = IndexList::new();
        HalfKP::append_active_indices(&pos, Color::Black, &mut active_b);
        let mut active_w: IndexList<MAX_ACTIVE_FEATURES> = IndexList::new();
        HalfKP::append_active_indices(&pos, Color::White, &mut active_w);

        eprintln!("Black perspective: {} features", active_b.len());
        eprintln!("White perspective: {} features", active_w.len());

        // インデックスの範囲確認
        let max_b = active_b.iter().max().copied().unwrap_or(0);
        let max_w = active_w.iter().max().copied().unwrap_or(0);
        let max_valid = 81 * FE_END - 1;
        eprintln!("Max index (Black): {}", max_b);
        eprintln!("Max index (White): {}", max_w);
        eprintln!("Max valid index: {}", max_valid);

        assert!(max_b <= max_valid, "Black max index out of range");
        assert!(max_w <= max_valid, "White max index out of range");
    }
}
