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

use super::Feature;
use super::TriggerEvent;
use crate::nnue::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use crate::nnue::bona_piece::BonaPiece;
use crate::nnue::bona_piece_halfka_hm::{halfka_index, is_hm_mirror, king_bucket, pack_bonapiece};
use crate::nnue::constants::HALFKA_HM_DIMENSIONS;
use crate::nnue::piece_list::PieceNumber;
use crate::position::Position;
use crate::types::{Color, Square};

/// HalfKA_hm^ 特徴量
///
/// キングバケット（Half-Mirror）とFactorizationを組み合わせた特徴量。
/// 自玉が動いた場合にアキュムレータの全計算が必要になる。
#[allow(non_camel_case_types)]
pub struct HalfKA_hm;

impl Feature for HalfKA_hm {
    /// 特徴量の次元数: BASE (45×1629) = 73,305
    const DIMENSIONS: usize = HALFKA_HM_DIMENSIONS;

    /// 同時にアクティブになる最大数（初期局面での値）
    ///
    /// 初期局面（手駒なし）: 盤上38駒 + 両王2 = 40
    /// 手駒がある場合: 40 + 手駒枚数（各手駒が1特徴量ずつ追加される）
    ///
    /// 例: 歩3枚を持っている場合 → 「歩1枚目」「歩2枚目」「歩3枚目」の3特徴量
    ///
    /// 実際のIndexListは`MAX_ACTIVE_FEATURES = 54`を使用し、安全マージンを持つ。
    const MAX_ACTIVE: usize = 40;

    /// 自玉が動いた場合に全計算
    const REFRESH_TRIGGER: TriggerEvent = TriggerEvent::FriendKingMoved;

    /// アクティブな特徴量インデックスを追記
    ///
    /// PieceList の全40エントリを走査（玉含む）。
    /// coalesce 済みモデル専用のため、Factor 特徴量は追加しない。
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
    ///
    /// DirtyPiece の ExtBonaPiece を直接使用し pack_bonapiece を適用。
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
    use crate::nnue::accumulator::{ChangedBonaPiece, DirtyPiece};
    use crate::nnue::bona_piece::ExtBonaPiece;
    use crate::nnue::constants::BASE_INPUTS_HALFKA;
    use crate::nnue::piece_list::PieceNumber;
    use crate::position::Position;
    use crate::types::{Color, File, Piece, PieceType, Rank, Square};

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
        // 駒取り: 2四の歩（先手）が2三に進んで2三の歩（後手）を取る
        let sq_24 = Square::new(File::File2, Rank::Rank4);
        let sq_23 = Square::new(File::File2, Rank::Rank3);
        let king_sq = Square::new(File::File5, Rank::Rank9);

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

        HalfKA_hm::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 新API: 各 changed_piece エントリが old/new ペアを持つ
        // [0]: 移動駒 old(2四)→new(2三) → removed=1, added=1
        // [1]: 捕獲駒 old(盤上2三)→new(手駒) → removed=1, added=1
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 2);
        assert_eq!(added.len(), 2);
    }

    #[test]
    fn test_append_changed_indices_hand_change() {
        // 手駒変化: 歩を1枚取得（0→1）
        // 新APIではChangedBonaPieceで表現
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::ZERO, // 手駒0枚→ZEROとして表現
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
        };

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKA_hm::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 手駒変化: removed=0（old=ZERO→フィルタ）, added=1（new=手駒1枚目BonaPiece）
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 0);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn test_append_active_indices_with_hand_pieces() {
        // 手駒が PieceList 経由で正しく特徴量に反映されることを確認
        let mut pos = Position::new();
        // 合法局面: 後手から歩3枚・香1枚を取得（盤上36 + 手駒4 = 40駒）
        pos.set_sfen("1nsgkgsnl/1r5b1/pppppp3/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 3P1L 1")
            .unwrap();

        let mut active = IndexList::new();
        HalfKA_hm::append_active_indices(&pos, Color::Black, &mut active);

        // PieceList の全40エントリ（盤上36 + 手駒4）が特徴量として追加される
        assert_eq!(active.len(), 40, "手駒の枚数分すべての特徴量が追加されるべき");
    }

    #[test]
    fn test_append_active_indices_multiple_hand_pieces() {
        // より多くの手駒が PieceList 経由で正しく反映されることを確認
        let mut pos = Position::new();
        // 合法局面: 後手から歩5枚・桂2枚・銀1枚を取得（盤上32 + 手駒8 = 40駒）
        pos.set_sfen("l2gkgs1l/1r5b1/pppp5/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 5P2N1S 1")
            .unwrap();

        let mut active = IndexList::new();
        HalfKA_hm::append_active_indices(&pos, Color::Black, &mut active);

        // PieceList の全40エントリ（盤上32 + 手駒8）が特徴量として追加される
        assert_eq!(active.len(), 40, "手駒の枚数分すべての特徴量が追加されるべき");
    }

    #[test]
    fn test_append_changed_indices_hand_increase() {
        // 手駒増加（1枚→2枚）
        // 新APIではold/new ExtBonaPieceペアとして表現
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 2),
        };

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKA_hm::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 手駒1→2: removed=1（1枚目のBonaPiece）, added=1（2枚目のBonaPiece）
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 1, "旧手駒BonaPieceを削除");
        assert_eq!(added.len(), 1, "新手駒BonaPieceを追加");
    }

    #[test]
    fn test_append_changed_indices_hand_decrease() {
        // 手駒減少（2枚→1枚）
        // 新APIではold/new ExtBonaPieceペアとして表現
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 2),
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
        };

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKA_hm::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 手駒2→1: removed=1（2枚目のBonaPiece）, added=1（1枚目のBonaPiece）
        // coalesce済みモデルではFactorization無し
        assert_eq!(removed.len(), 1, "旧手駒BonaPieceを削除");
        assert_eq!(added.len(), 1, "新手駒BonaPieceを追加");
    }

    #[test]
    fn test_append_changed_indices_hand_increase_multiple() {
        // 手駒が0枚→3枚に増加
        // 新APIでは1回の手駒変化は1エントリのold/newペア
        // 0→3の一括変化は通常do_moveでは発生しないが、テストとして記述
        let king_sq = Square::new(File::File5, Rank::Rank9);

        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::ZERO, // 手駒0枚→ZEROとして表現
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 3),
        };

        let mut removed = IndexList::new();
        let mut added = IndexList::new();

        HalfKA_hm::append_changed_indices(
            &dirty_piece,
            Color::Black,
            king_sq,
            &mut removed,
            &mut added,
        );

        // 新API: 1エントリのold(ZERO)/new(3枚目)ペア
        // removed=0（old=ZERO→フィルタ）, added=1（new=3枚目BonaPiece）
        assert_eq!(removed.len(), 0);
        assert_eq!(added.len(), 1, "3枚目のBonaPieceを追加");
    }

    #[test]
    fn test_append_changed_indices_enemy_king_move() {
        // 相手の王の移動: 5一→4一
        // HalfKA_hmでは相手の王も特徴量に含めるため、差分更新で処理される
        let sq_51 = Square::new(File::File5, Rank::Rank1);
        let sq_41 = Square::new(File::File4, Rank::Rank1);
        let king_sq = Square::new(File::File5, Rank::Rank9); // 自玉は5九

        let old_bp = ExtBonaPiece::from_board(Piece::W_KING, sq_51);
        let new_bp = ExtBonaPiece::from_board(Piece::W_KING, sq_41);
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: old_bp,
            new_piece: new_bp,
        };

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
