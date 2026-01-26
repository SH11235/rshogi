//! HalfKA_hm^ 特徴量
//!
//! Half-Mirror King + All pieces (coalesced)
//!
//! 主な特徴:
//! - キングバケット: 45バケット（Half-Mirror: 9段 × 5筋）
//! - 入力次元: 73,305 (BASE: 45×1629)
//!
//! 注意: nnue-pytorchのcoalesce済みモデル専用。
//! Factorizationの重みはBase側に畳み込み済みのため、推論時はBaseのみで計算する。
//!
//! 参考実装: nnue-pytorch training_data_loader.cpp, serialize.py

use super::{Feature, TriggerEvent, BOARD_PIECE_TYPES};
use crate::nnue::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use crate::nnue::bona_piece::{BonaPiece, PIECE_BASE};
use crate::nnue::bona_piece_halfka_hm::{
    halfka_index, is_hm_mirror, king_bonapiece, king_bucket, pack_bonapiece,
};
use crate::nnue::constants::HALFKA_HM_DIMENSIONS;
use crate::position::Position;
use crate::types::{Color, PieceType, Square};

/// HalfKA_hm^ 特徴量
///
/// キングバケット（Half-Mirror）とFactorizationを組み合わせた特徴量。
/// 自玉が動いた場合にアキュムレータの全計算が必要になる。
#[allow(non_camel_case_types)]
pub struct HalfKA_hm;

impl Feature for HalfKA_hm {
    /// 特徴量の次元数: BASE (45×1629) = 73,305
    const DIMENSIONS: usize = HALFKA_HM_DIMENSIONS;

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
    /// 各駒について:
    /// Base特徴: king_bucket * PIECE_INPUTS + pack(bp, hm_mirror)
    ///
    /// 注意: coalesce済みモデル専用のため、Factor特徴量は追加しない。
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

                    // Base特徴量（coalesce済みモデルではこれのみ）
                    // 合法局面では溢れないため戻り値を無視
                    let _ = active.push(halfka_index(kb, packed));
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
        // 合法局面では溢れないため戻り値を無視
        let _ = active.push(halfka_index(kb, packed_friend_king));

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
        // 合法局面では溢れないため戻り値を無視
        let _ = active.push(halfka_index(kb, packed_enemy_king));

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

                        // Base特徴量（coalesce済みモデルではこれのみ）
                        // 合法局面では溢れないため戻り値を無視
                        let _ = active.push(halfka_index(kb, packed));
                    }
                }
            }
        }
    }

    /// 変化した特徴量インデックスを追記
    ///
    /// DirtyPieceから変化した特徴量を計算する。
    /// coalesce済みモデル専用のため、Base特徴量のみを処理。
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
                        // 合法局面では溢れないため戻り値を無視
                        let _ = removed.push(halfka_index(kb, packed));
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
                        // 合法局面では溢れないため戻り値を無視
                        let _ = added.push(halfka_index(kb, packed));
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
                        // 合法局面では溢れないため戻り値を無視
                        let _ = added.push(halfka_index(kb, packed));
                    }
                }
            } else if hc.old_count > hc.new_count {
                // 枚数減少: new_count+1 から old_count までの特徴量を削除
                for i in (hc.new_count + 1)..=hc.old_count {
                    let bp = BonaPiece::from_hand_piece(perspective, hc.owner, hc.piece_type, i);
                    if bp != BonaPiece::ZERO {
                        let packed = pack_bonapiece(bp, hm_mirror);
                        // 合法局面では溢れないため戻り値を無視
                        let _ = removed.push(halfka_index(kb, packed));
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
    use crate::nnue::constants::BASE_INPUTS_HALFKA;
    use crate::position::Position;
    use crate::types::{File, Piece, Rank};

    #[test]
    fn test_halfka_hm_dimensions() {
        // coalesce済みモデル: BASE (45×1629) = 73,305
        assert_eq!(HalfKA_hm::DIMENSIONS, 73_305);
        assert_eq!(HalfKA_hm::DIMENSIONS, BASE_INPUTS_HALFKA);
    }

    #[test]
    fn test_halfka_hm_max_active() {
        // coalesce済みモデルではFactorization無し
        // 合法局面では盤上駒 + 手駒 + 両王 = 40駒
        assert_eq!(HalfKA_hm::MAX_ACTIVE, 40);
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

        // 初期局面: 盤上38駒 + 両方の王2 = 40
        // coalesce済みモデルではFactorization無し
        assert_eq!(active.len(), 40);
    }

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

        HalfKA_hm::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 1駒移動: removed=1 (base), added=1 (base)
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 1);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn test_append_changed_indices_capture() {
        // 駒取り
        let sq_24 = Square::new(File::File2, Rank::Rank4);
        let sq_23 = Square::new(File::File2, Rank::Rank3);
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();

        // 動いた駒（先手の歩）
        let _ = dirty_piece.push_piece(ChangedPiece {
            color: Color::Black,
            old_piece: Piece::B_PAWN,
            old_sq: Some(sq_24),
            new_piece: Piece::B_PAWN,
            new_sq: Some(sq_23),
        });

        // 取られた駒（後手の歩）
        let _ = dirty_piece.push_piece(ChangedPiece {
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

        // 2駒がremoved（元位置の歩 + 取られた歩）= 2
        // 1駒がadded（新位置の歩）= 1
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 2);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn test_append_changed_indices_hand_change() {
        // 手駒変化
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();

        let _ = dirty_piece.push_hand_change(HandChange {
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

        // 手駒変化: removed=0, added=1 (base)
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 0);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn test_append_active_indices_with_hand_pieces() {
        // 手駒の枚数分すべての特徴量が追加されることを確認
        let mut pos = Position::new();
        // 先手が歩3枚、香1枚を持っている局面
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 3P1L 1")
            .unwrap();

        let mut active = IndexList::new();
        HalfKA_hm::append_active_indices(&pos, Color::Black, &mut active);

        // 盤上38駒 + 両方の王2 = 40
        // 手駒: 歩3枚(3) + 香1枚(1) = 4
        // 合計 = 40 + 4 = 44
        // coalesce済みモデルではFactorization無し
        assert_eq!(active.len(), 44, "手駒の枚数分すべての特徴量が追加されるべき");
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

        // 盤上38駒 + 両方の王2 = 40
        // 手駒: 歩5枚(5) + 桂2枚(2) + 銀1枚(1) = 8
        // 合計 = 40 + 8 = 48
        // coalesce済みモデルではFactorization無し
        assert_eq!(active.len(), 48, "手駒の枚数分すべての特徴量が追加されるべき");
    }

    #[test]
    fn test_append_changed_indices_hand_increase() {
        // 手駒増加（1枚→2枚）: 2枚目の特徴量だけを追加し、1枚目は維持
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();

        let _ = dirty_piece.push_hand_change(HandChange {
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

        // 増加時: removed=0（既存の1枚目は維持）, added=1（2枚目のbase）
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 0, "増加時は既存の特徴量を削除しない");
        assert_eq!(added.len(), 1, "増加分の特徴量だけを追加");
    }

    #[test]
    fn test_append_changed_indices_hand_decrease() {
        // 手駒減少（2枚→1枚）: 2枚目の特徴量だけを削除し、1枚目は維持
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();

        let _ = dirty_piece.push_hand_change(HandChange {
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

        // 減少時: removed=1（2枚目のbase）, added=0（1枚目は維持）
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 1, "減少分の特徴量だけを削除");
        assert_eq!(added.len(), 0, "減少時は特徴量を追加しない");
    }

    #[test]
    fn test_append_changed_indices_hand_increase_multiple() {
        // 手駒が0枚→3枚に増加: 1枚目、2枚目、3枚目すべて追加
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();

        let _ = dirty_piece.push_hand_change(HandChange {
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

        // 0→3: removed=0, added=3（3枚分のbase）
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 0);
        assert_eq!(added.len(), 3, "3枚分の特徴量を追加");
    }

    #[test]
    fn test_append_changed_indices_enemy_king_move() {
        // 相手の王の移動: 5一→4一
        // HalfKA_hmでは相手の王も特徴量に含めるため、差分更新で処理される
        let sq_51 = Square::new(File::File5, Rank::Rank1);
        let sq_41 = Square::new(File::File4, Rank::Rank1);
        let king_sq = Square::new(File::File5, Rank::Rank9); // 自玉は5九

        let mut dirty_piece = DirtyPiece::new();
        let _ = dirty_piece.push_piece(ChangedPiece {
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

        // 相手の王の移動: removed=1 (base), added=1 (base)
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 1, "相手の王の旧位置の特徴量を削除");
        assert_eq!(added.len(), 1, "相手の王の新位置の特徴量を追加");
    }
}
