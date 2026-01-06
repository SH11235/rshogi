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
    factorized_index, halfka_index, is_hm_mirror, king_bonapiece, king_bucket, pack_bonapiece,
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

    /// 同時にアクティブになる最大数: (盤上38駒 + 両方の王2 + 手駒14) × 2 (factorized) = 108
    /// ※ 実際は40駒程度なので80くらいが現実的
    const MAX_ACTIVE: usize = 108;

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

        // 盤上の駒（駒種・色ごとにループ）- 王以外
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

        // 両方の王の特徴量を追加
        // nnue-pytorchのtraining_data_loader.cppでは、EvalList内の全40駒
        // （両方の王を含む）を特徴量として使用している。
        // 自玉の位置はking_bucketにも反映されるが、特徴量としても追加する。

        // 自玉の特徴量
        let friend_king_sq_index = if perspective == Color::Black {
            king_sq.index()
        } else {
            king_sq.inverse().index()
        };
        let friend_king_bp = king_bonapiece(friend_king_sq_index, true); // is_friend=true
        let packed_friend_king = pack_bonapiece(friend_king_bp, hm_mirror);
        active.push(halfka_index(kb, packed_friend_king));
        active.push(factorized_index(packed_friend_king));

        // 敵玉の特徴量
        let enemy = perspective.opponent();
        let enemy_king_sq = pos.king_square(enemy);
        let enemy_king_sq_index = if perspective == Color::Black {
            enemy_king_sq.index()
        } else {
            enemy_king_sq.inverse().index()
        };
        let enemy_king_bp = king_bonapiece(enemy_king_sq_index, false); // is_friend=false
        let packed_enemy_king = pack_bonapiece(enemy_king_bp, hm_mirror);
        active.push(halfka_index(kb, packed_enemy_king));
        active.push(factorized_index(packed_enemy_king));

        // 手駒の特徴量
        // HalfKPと同様に、手駒の枚数分すべての特徴量を追加する
        // 例: 歩を3枚持っている場合、1枚目・2枚目・3枚目の特徴量をそれぞれ追加
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
                // 手駒の枚数分、すべての特徴量を追加
                for i in 1..=count {
                    let bp = BonaPiece::from_hand_piece(perspective, owner, pt, i);
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
                    // 視点に応じてマスを変換
                    let sq_index = if perspective == Color::Black {
                        sq.index()
                    } else {
                        sq.inverse().index()
                    };

                    // 王の場合は king_bonapiece を使用
                    // (BonaPiece::from_piece_square は King に対して ZERO を返すため)
                    let bp = if dp.old_piece.piece_type() == PieceType::King {
                        let is_friend = dp.color == perspective;
                        king_bonapiece(sq_index, is_friend)
                    } else {
                        BonaPiece::from_piece_square(dp.old_piece, sq, perspective)
                    };

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
                    // 視点に応じてマスを変換
                    let sq_index = if perspective == Color::Black {
                        sq.index()
                    } else {
                        sq.inverse().index()
                    };

                    // 王の場合は king_bonapiece を使用
                    let bp = if dp.new_piece.piece_type() == PieceType::King {
                        let is_friend = dp.color == perspective;
                        king_bonapiece(sq_index, is_friend)
                    } else {
                        BonaPiece::from_piece_square(dp.new_piece, sq, perspective)
                    };

                    if bp != BonaPiece::ZERO {
                        let packed = pack_bonapiece(bp, hm_mirror);
                        added.push(halfka_index(kb, packed));
                        added.push(factorized_index(packed));
                    }
                }
            }
        }

        // 手駒の変化を反映
        // HalfKA_hmでは手駒の枚数分すべての特徴量がアクティブになる設計
        // 例: 歩3枚 → 歩1枚目、歩2枚目、歩3枚目の3つの特徴量がすべてアクティブ
        // したがって差分更新では:
        // - 増加時: 増えた分だけ追加（既存はそのまま維持）
        // - 減少時: 減った分だけ削除（残る分は維持）
        for hc in dirty_piece.hand_changes() {
            if hc.old_count < hc.new_count {
                // 枚数増加: old_count+1 から new_count までの特徴量を追加
                for i in (hc.old_count + 1)..=hc.new_count {
                    let bp = BonaPiece::from_hand_piece(perspective, hc.owner, hc.piece_type, i);
                    if bp != BonaPiece::ZERO {
                        let packed = pack_bonapiece(bp, hm_mirror);
                        added.push(halfka_index(kb, packed));
                        added.push(factorized_index(packed));
                    }
                }
            } else if hc.old_count > hc.new_count {
                // 枚数減少: new_count+1 から old_count までの特徴量を削除
                for i in (hc.new_count + 1)..=hc.old_count {
                    let bp = BonaPiece::from_hand_piece(perspective, hc.owner, hc.piece_type, i);
                    if bp != BonaPiece::ZERO {
                        let packed = pack_bonapiece(bp, hm_mirror);
                        removed.push(halfka_index(kb, packed));
                        removed.push(factorized_index(packed));
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
        // 最大54駒（盤上38 + 両方の王2 + 手駒14）× 2 = 108
        assert_eq!(HalfKA_hm::MAX_ACTIVE, 108);
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

        // 初期局面: (盤上38駒 + 両方の王2) × 2 (factorized) = 80
        assert_eq!(active.len(), 80);
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

    #[test]
    fn test_append_active_indices_with_hand_pieces() {
        // 手駒が複数枚ある局面のテスト
        // P1バグ修正の検証: 手駒の枚数分すべての特徴量が追加されることを確認
        let mut pos = Position::new();
        // 先手が歩3枚、香1枚を持っている局面
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 3P1L 1")
            .unwrap();

        let mut active = IndexList::new();
        HalfKA_hm::append_active_indices(&pos, Color::Black, &mut active);

        // (盤上38駒 + 両方の王2) × 2 = 80
        // 手駒: 歩3枚(3×2=6) + 香1枚(1×2=2) = 8
        // 合計 = 80 + 8 = 88
        assert_eq!(active.len(), 88, "手駒の枚数分すべての特徴量が追加されるべき");
    }

    #[test]
    fn test_append_active_indices_multiple_hand_pieces() {
        // より多くの手駒がある局面
        let mut pos = Position::new();
        // 先手が歩5枚、桂2枚、銀1枚を持っている局面
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 5P2N1S 1")
            .unwrap();

        let mut active = IndexList::new();
        HalfKA_hm::append_active_indices(&pos, Color::Black, &mut active);

        // (盤上38駒 + 両方の王2) × 2 = 80
        // 手駒: 歩5枚(5×2=10) + 桂2枚(2×2=4) + 銀1枚(1×2=2) = 16
        // 合計 = 80 + 16 = 96
        assert_eq!(active.len(), 96, "手駒の枚数分すべての特徴量が追加されるべき");
    }

    #[test]
    fn test_append_changed_indices_hand_increase() {
        // 手駒増加（1枚→2枚）: 2枚目の特徴量だけを追加し、1枚目は維持
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();

        dirty_piece.push_hand_change(HandChange {
            owner: Color::Black,
            piece_type: PieceType::Pawn,
            old_count: 1,
            new_count: 2,
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

        // 増加時: removed=0（既存の1枚目は維持）, added=2（2枚目のbase+factor）
        assert_eq!(removed.len(), 0, "増加時は既存の特徴量を削除しない");
        assert_eq!(added.len(), 2, "増加分の特徴量だけを追加");
    }

    #[test]
    fn test_append_changed_indices_hand_decrease() {
        // 手駒減少（2枚→1枚）: 2枚目の特徴量だけを削除し、1枚目は維持
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();

        dirty_piece.push_hand_change(HandChange {
            owner: Color::Black,
            piece_type: PieceType::Pawn,
            old_count: 2,
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

        // 減少時: removed=2（2枚目のbase+factor）, added=0（1枚目は維持）
        assert_eq!(removed.len(), 2, "減少分の特徴量だけを削除");
        assert_eq!(added.len(), 0, "減少時は特徴量を追加しない");
    }

    #[test]
    fn test_append_changed_indices_hand_increase_multiple() {
        // 手駒が0枚→3枚に増加: 1枚目、2枚目、3枚目すべて追加
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();

        dirty_piece.push_hand_change(HandChange {
            owner: Color::Black,
            piece_type: PieceType::Pawn,
            old_count: 0,
            new_count: 3,
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

        // 0→3: removed=0, added=6（3枚分 × 2 (base+factor)）
        assert_eq!(removed.len(), 0);
        assert_eq!(added.len(), 6, "3枚分の特徴量を追加");
    }

    #[test]
    fn test_append_changed_indices_enemy_king_move() {
        // 相手の王の移動: 5一→4一
        // HalfKA_hmでは相手の王も特徴量に含めるため、差分更新で処理される
        let sq_51 = Square::new(File::File5, Rank::Rank1);
        let sq_41 = Square::new(File::File4, Rank::Rank1);
        let king_sq = Square::new(File::File5, Rank::Rank9); // 自玉は5九

        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.push_piece(ChangedPiece {
            color: Color::White, // 相手（後手）の王
            old_piece: Piece::W_KING,
            old_sq: Some(sq_51),
            new_piece: Piece::W_KING,
            new_sq: Some(sq_41),
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

        // 相手の王の移動: removed=2 (base+factor), added=2 (base+factor)
        assert_eq!(removed.len(), 2, "相手の王の旧位置の特徴量を削除");
        assert_eq!(added.len(), 2, "相手の王の新位置の特徴量を追加");
    }
}
