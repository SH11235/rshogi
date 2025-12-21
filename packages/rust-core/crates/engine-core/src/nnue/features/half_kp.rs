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
        let king_sq = pos.king_square(perspective);

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
                    active.push(index);
                }
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
            if dp.old_piece.is_some() {
                if let Some(sq) = dp.old_sq {
                    let bp = BonaPiece::from_piece_square(dp.old_piece, sq, perspective);
                    if bp != BonaPiece::ZERO {
                        removed.push(halfkp_index(king_sq, bp));
                    }
                }
            }

            // 盤上に現れる側（new）
            if dp.new_piece.is_some() {
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
        dirty_piece.push_piece(ChangedPiece {
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
        dirty_piece.push_piece(ChangedPiece {
            color: Color::Black,
            old_piece: Piece::B_PAWN,
            old_sq: Some(sq_24),
            new_piece: Piece::B_PAWN,
            new_sq: Some(sq_23),
        });

        // 取られた駒（後手の歩）- 盤上から消える
        dirty_piece.push_piece(ChangedPiece {
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
        dirty_piece.push_piece(ChangedPiece {
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
        dirty_piece.push_hand_change(HandChange {
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

        dirty_piece.push_hand_change(HandChange {
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

        // 手駒が1→2に変化: removed=1（old特徴量）, added=1（new特徴量）
        assert_eq!(removed.len(), 1);
        assert_eq!(added.len(), 1);

        // インデックスは異なるはず（枚数が異なるので）
        let removed_idx: Vec<_> = removed.iter().copied().collect();
        let added_idx: Vec<_> = added.iter().copied().collect();
        assert_ne!(removed_idx[0], added_idx[0]);
    }
}
