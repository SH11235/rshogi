//! HalfKP 特徴量
//!
//! 自玉位置×駒配置（BonaPiece）の組み合わせで特徴量を表現する。
//! YaneuraOu の HalfKP<Friend> に相当する。

use super::{Feature, TriggerEvent};
use crate::nnue::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use crate::nnue::bona_piece::{bona_piece_from_base, halfkp_index, BonaPiece, FE_END, PIECE_BASE};
use crate::position::Position;
use crate::types::{Color, PieceType, Square};

/// 盤上の駒種（King除外）
/// append_active_indices で使用する定数配列
const BOARD_PIECE_TYPES: [PieceType; 13] = [
    PieceType::Pawn,
    PieceType::Lance,
    PieceType::Knight,
    PieceType::Silver,
    PieceType::Gold,
    PieceType::Bishop,
    PieceType::Rook,
    PieceType::ProPawn,
    PieceType::ProLance,
    PieceType::ProKnight,
    PieceType::ProSilver,
    PieceType::Horse,
    PieceType::Dragon,
];

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
    /// bitboard 型別ループで高速化: piece_on() 呼び出しと match 分岐を削減。
    #[inline]
    fn append_active_indices(
        pos: &Position,
        perspective: Color,
        active: &mut IndexList<MAX_ACTIVE_FEATURES>,
    ) {
        // 玉のマスを取得（後手視点では反転）
        // やねうら王と同様に、視点に応じたマス表現を使用
        let raw_king_sq = pos.king_square(perspective);
        let king_sq = if perspective == Color::Black {
            raw_king_sq
        } else {
            raw_king_sq.inverse()
        };

        // 盤上の駒（駒種・色ごとにループ）
        for color in [Color::Black, Color::White] {
            let is_friend = (color == perspective) as usize;

            for &pt in &BOARD_PIECE_TYPES {
                let base = PIECE_BASE[pt as usize][is_friend];
                let bb = pos.pieces(color, pt);

                for sq in bb.iter() {
                    let sq_index = if perspective == Color::Black {
                        sq.index()
                    } else {
                        sq.inverse().index()
                    };
                    let bp = bona_piece_from_base(sq_index, base);
                    let index = halfkp_index(king_sq, bp);
                    // 合法局面では溢れないため戻り値を無視
                    let _ = active.push(index);
                }
            }
        }

        // 手駒の特徴量
        // やねうら王では、手駒は「枚数分すべて」の特徴量を追加する
        // 例: 歩を3枚持っている場合、1枚目・2枚目・3枚目の3つの特徴量を追加
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
                // 手駒の枚数分、すべての特徴量を追加
                for i in 1..=count {
                    let bp = BonaPiece::from_hand_piece(perspective, owner, pt, i);
                    if bp != BonaPiece::ZERO {
                        let index = halfkp_index(king_sq, bp);
                        // 合法局面では溢れないため戻り値を無視
                        let _ = active.push(index);
                    }
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
            if dp.old_piece.is_some() {
                if let Some(sq) = dp.old_sq {
                    let bp = BonaPiece::from_piece_square(dp.old_piece, sq, perspective);
                    if bp != BonaPiece::ZERO {
                        // 合法局面では溢れないため戻り値を無視
                        let _ = removed.push(halfkp_index(king_sq, bp));
                    }
                }
            }

            // 盤上に現れる側（new）
            if dp.new_piece.is_some() {
                if let Some(sq) = dp.new_sq {
                    let bp = BonaPiece::from_piece_square(dp.new_piece, sq, perspective);
                    if bp != BonaPiece::ZERO {
                        // 合法局面では溢れないため戻り値を無視
                        let _ = added.push(halfkp_index(king_sq, bp));
                    }
                }
            }
        }

        // 手駒の変化を反映
        // やねうら王では、手駒の変化は差分のみを処理する
        // 例: 2枚→3枚: 3枚目を追加、3枚→2枚: 3枚目を削除
        for hc in dirty_piece.hand_changes() {
            if hc.old_count < hc.new_count {
                // 増えた分を追加 (old_count+1 から new_count まで)
                for i in (hc.old_count + 1)..=hc.new_count {
                    let bp = BonaPiece::from_hand_piece(perspective, hc.owner, hc.piece_type, i);
                    if bp != BonaPiece::ZERO {
                        // 合法局面では溢れないため戻り値を無視
                        let _ = added.push(halfkp_index(king_sq, bp));
                    }
                }
            } else if hc.old_count > hc.new_count {
                // 減った分を削除 (new_count+1 から old_count まで)
                for i in (hc.new_count + 1)..=hc.old_count {
                    let bp = BonaPiece::from_hand_piece(perspective, hc.owner, hc.piece_type, i);
                    if bp != BonaPiece::ZERO {
                        // 合法局面では溢れないため戻り値を無視
                        let _ = removed.push(halfkp_index(king_sq, bp));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::accumulator::{ChangedPiece, HandChange};
    use crate::position::Position;
    use crate::types::{File, Piece, Rank};

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

        let mut dirty_piece = DirtyPiece::new();
        let _ = dirty_piece.push_piece(ChangedPiece {
            color: Color::Black,
            old_piece: Piece::B_PAWN,
            old_sq: Some(sq_77),
            new_piece: Piece::B_PAWN,
            new_sq: Some(sq_76),
        });

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

        // 動いた駒（先手の歩）
        let _ = dirty_piece.push_piece(ChangedPiece {
            color: Color::Black,
            old_piece: Piece::B_PAWN,
            old_sq: Some(sq_24),
            new_piece: Piece::B_PAWN,
            new_sq: Some(sq_23),
        });

        // 取られた駒（後手の歩）- 盤上から消える
        let _ = dirty_piece.push_piece(ChangedPiece {
            color: Color::White,
            old_piece: Piece::W_PAWN,
            old_sq: Some(sq_23),
            new_piece: Piece::NONE,
            new_sq: None,
        });

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKP::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 2駒がremoved（元位置の歩 + 取られた歩）, 1駒がadded（新位置の歩）
        assert_eq!(removed.len(), 2);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn test_append_changed_indices_drop() {
        // 打ち込み: 手駒から盤上へ
        let king_sq = Square::new(File::File5, Rank::Rank9); // 5九

        let mut dirty_piece = DirtyPiece::new();

        // 打った駒（盤上に現れる）
        let _ = dirty_piece.push_piece(ChangedPiece {
            color: Color::Black,
            old_piece: Piece::NONE,
            old_sq: None,
            new_piece: Piece::B_PAWN,
            new_sq: Some(Square::SQ_55),
        });

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKP::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 打ち込み: removed=0（盤上駒の変化なし）, added=1
        assert_eq!(removed.len(), 0);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn test_append_changed_indices_hand_change() {
        // 手駒変化: 取った駒が手駒になる
        let king_sq = Square::new(File::File5, Rank::Rank9); // 5九

        let mut dirty_piece = DirtyPiece::new();

        // 手駒変化（歩を1枚取得: 0 → 1）
        let _ = dirty_piece.push_hand_change(HandChange {
            owner: Color::Black,
            piece_type: PieceType::Pawn,
            old_count: 0,
            new_count: 1,
        });

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKP::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 手駒変化: removed=0（old_count=0）, added=1（new_count=1）
        assert_eq!(removed.len(), 0);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn test_append_changed_indices_hand_change_increment() {
        // 手駒変化: 既存の手駒が増える（1 → 2）
        let king_sq = Square::new(File::File5, Rank::Rank9); // 5九

        let mut dirty_piece = DirtyPiece::new();

        let _ = dirty_piece.push_hand_change(HandChange {
            owner: Color::Black,
            piece_type: PieceType::Pawn,
            old_count: 1,
            new_count: 2,
        });

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKP::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 手駒が1→2に変化: removed=0（1枚目は有効なまま）, added=1（2枚目を追加）
        // YaneuraOu形式では、2枚持っている場合は1枚目と2枚目の両方の特徴量が有効
        assert_eq!(removed.len(), 0);
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
