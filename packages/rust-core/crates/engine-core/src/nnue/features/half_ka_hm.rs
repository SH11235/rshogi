//! HalfKA_hm^ 特徴量
//!
//! Half-Mirror King + All pieces with Factorization
//!
//! 主な特徴:
//! - キングバケット: 45バケット（Half-Mirror: 9段 × 5筋）
//! - Factorization: 各駒に2つの特徴量（base + factor）
//! - 入力次元: 74,934 (BASE: 73,305 + FACT: 1,629)
//!
//! 参考実装: nnue-pytorch training_data_loader.cpp

use super::{Feature, TriggerEvent};
use crate::nnue::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use crate::nnue::bona_piece::{BonaPiece, PIECE_BASE};
use crate::nnue::bona_piece_halfka::{
    factorized_index, halfka_index, is_hm_mirror, king_bucket, pack_bonapiece,
};
use crate::nnue::constants::HALFKA_HM_DIMENSIONS;
use crate::position::Position;
use crate::types::{Color, PieceType, Square};

/// 盤上の駒種（King除外）
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

/// HalfKA_hm^ 特徴量
///
/// キングバケット（Half-Mirror）とFactorizationを組み合わせた特徴量。
/// 自玉が動いた場合にアキュムレータの全計算が必要になる。
#[allow(non_camel_case_types)]
pub struct HalfKA_hm;

impl Feature for HalfKA_hm {
    /// 特徴量の次元数: BASE (45×1629) + FACT (1629) = 74,934
    const DIMENSIONS: usize = HALFKA_HM_DIMENSIONS;

    /// 同時にアクティブになる最大数: (盤上38駒 + 手駒14) × 2 (factorized) = 104
    /// ※ 実際は40駒程度なので80くらいが現実的
    const MAX_ACTIVE: usize = 104;

    /// 自玉が動いた場合に全計算
    const REFRESH_TRIGGER: TriggerEvent = TriggerEvent::FriendKingMoved;

    /// アクティブな特徴量インデックスを追記
    ///
    /// 各駒について:
    /// 1. Base特徴: king_bucket * PIECE_INPUTS + pack(bp, hm_mirror)
    /// 2. Factor特徴: BASE_INPUTS + pack(bp, hm_mirror)
    #[inline]
    fn append_active_indices(
        pos: &Position,
        perspective: Color,
        active: &mut IndexList<MAX_ACTIVE_FEATURES>,
    ) {
        let king_sq = pos.king_square(perspective);
        let kb = king_bucket(king_sq, perspective);
        let hm_mirror = is_hm_mirror(king_sq, perspective);

        // 盤上の駒（駒種・色ごとにループ）
        for color in [Color::Black, Color::White] {
            let is_friend = (color == perspective) as usize;

            for &pt in &BOARD_PIECE_TYPES {
                let base = PIECE_BASE[pt as usize][is_friend];
                let bb = pos.pieces(color, pt);

                for sq in bb.iter() {
                    // 視点に応じてマスを変換
                    let sq_index = if perspective == Color::Black {
                        sq.index()
                    } else {
                        sq.inverse().index()
                    };

                    // BonaPieceを生成
                    let bp = BonaPiece::new(base + sq_index as u16);

                    // pack_bonapieceでHalf-Mirror処理
                    let packed = pack_bonapiece(bp, hm_mirror);

                    // Base特徴量
                    active.push(halfka_index(kb, packed));

                    // Factorization特徴量
                    active.push(factorized_index(packed));
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
                    // 手駒はミラー不要
                    let packed = pack_bonapiece(bp, hm_mirror);

                    // Base特徴量
                    active.push(halfka_index(kb, packed));

                    // Factorization特徴量
                    active.push(factorized_index(packed));
                }
            }
        }
    }

    /// 変化した特徴量インデックスを追記
    ///
    /// DirtyPieceから変化した特徴量を計算する。
    /// Factorization対応のため、各変化に対して2つの特徴量（base + factor）を処理。
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

        // 盤上駒の変化を処理
        for dp in dirty_piece.pieces() {
            // 盤上から消える側（old）
            if dp.old_piece.is_some() {
                if let Some(sq) = dp.old_sq {
                    let bp = BonaPiece::from_piece_square(dp.old_piece, sq, perspective);
                    if bp != BonaPiece::ZERO {
                        let packed = pack_bonapiece(bp, hm_mirror);
                        removed.push(halfka_index(kb, packed));
                        removed.push(factorized_index(packed));
                    }
                }
            }

            // 盤上に現れる側（new）
            if dp.new_piece.is_some() {
                if let Some(sq) = dp.new_sq {
                    let bp = BonaPiece::from_piece_square(dp.new_piece, sq, perspective);
                    if bp != BonaPiece::ZERO {
                        let packed = pack_bonapiece(bp, hm_mirror);
                        added.push(halfka_index(kb, packed));
                        added.push(factorized_index(packed));
                    }
                }
            }
        }

        // 手駒の変化を反映
        for hc in dirty_piece.hand_changes() {
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
                        let packed = pack_bonapiece(bp_old, hm_mirror);
                        removed.push(halfka_index(kb, packed));
                        removed.push(factorized_index(packed));
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
                        let packed = pack_bonapiece(bp_new, hm_mirror);
                        added.push(halfka_index(kb, packed));
                        added.push(factorized_index(packed));
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
    use crate::nnue::bona_piece_halfka::PIECE_INPUTS;
    use crate::nnue::constants::BASE_INPUTS_HALFKA;
    use crate::position::Position;
    use crate::types::{File, Piece, Rank};

    #[test]
    fn test_halfka_hm_dimensions() {
        assert_eq!(HalfKA_hm::DIMENSIONS, 74_934);
        assert_eq!(HalfKA_hm::DIMENSIONS, BASE_INPUTS_HALFKA + PIECE_INPUTS);
    }

    #[test]
    fn test_halfka_hm_max_active() {
        // 各駒で2つの特徴量（base + factor）
        // 最大52駒 × 2 = 104
        assert_eq!(HalfKA_hm::MAX_ACTIVE, 104);
    }

    #[test]
    fn test_halfka_hm_refresh_trigger() {
        assert_eq!(HalfKA_hm::REFRESH_TRIGGER, TriggerEvent::FriendKingMoved);
    }

    #[test]
    fn test_append_active_indices_startpos() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        let mut active = IndexList::new();

        HalfKA_hm::append_active_indices(&pos, Color::Black, &mut active);

        // 初期局面: 盤上38駒 × 2 (factorized) = 76
        assert_eq!(active.len(), 76);
    }

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

        HalfKA_hm::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 1駒移動: removed=2 (base+factor), added=2 (base+factor)
        assert_eq!(removed.len(), 2);
        assert_eq!(added.len(), 2);
    }

    #[test]
    fn test_append_changed_indices_capture() {
        // 駒取り
        let sq_24 = Square::new(File::File2, Rank::Rank4);
        let sq_23 = Square::new(File::File2, Rank::Rank3);
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();

        // 動いた駒（先手の歩）
        dirty_piece.push_piece(ChangedPiece {
            color: Color::Black,
            old_piece: Piece::B_PAWN,
            old_sq: Some(sq_24),
            new_piece: Piece::B_PAWN,
            new_sq: Some(sq_23),
        });

        // 取られた駒（後手の歩）
        dirty_piece.push_piece(ChangedPiece {
            color: Color::White,
            old_piece: Piece::W_PAWN,
            old_sq: Some(sq_23),
            new_piece: Piece::NONE,
            new_sq: None,
        });

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKA_hm::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 2駒がremoved（元位置の歩 + 取られた歩）× 2 (factor) = 4
        // 1駒がadded（新位置の歩）× 2 (factor) = 2
        assert_eq!(removed.len(), 4);
        assert_eq!(added.len(), 2);
    }

    #[test]
    fn test_append_changed_indices_hand_change() {
        // 手駒変化
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();

        dirty_piece.push_hand_change(HandChange {
            owner: Color::Black,
            piece_type: PieceType::Pawn,
            old_count: 0,
            new_count: 1,
        });

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKA_hm::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 手駒変化: removed=0, added=2 (base+factor)
        assert_eq!(removed.len(), 0);
        assert_eq!(added.len(), 2);
    }
}
