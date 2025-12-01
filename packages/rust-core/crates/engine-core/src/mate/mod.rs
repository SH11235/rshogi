// 1手詰め探索モジュール
// YaneuraOuのmate1ply_without_effect.cppの移植

pub mod drop_mate;
pub mod helpers;
pub mod move_mate;
pub mod tables;

use crate::bitboard::{
    bishop_effect, king_effect, lance_effect, line_bb, rook_effect, Bitboard, RANK_BB,
};
use crate::movegen::{self, MoveList};
use crate::position::Position;
use crate::types::{Color, Move, Square};

/// 成りが選択肢に入るか
#[inline]
pub fn can_promote(c: Color, from: Square, to: Square) -> bool {
    let enemy = enemy_field(c);
    enemy.contains(from) || enemy.contains(to)
}

/// 移動先が敵陣かどうか
#[inline]
pub fn can_promote_to(c: Color, to: Square) -> bool {
    enemy_field(c).contains(to)
}

/// 3点が一直線上に並ぶか
#[inline]
pub fn aligned(s1: Square, s2: Square, s3: Square) -> bool {
    let line = line_bb(s1, s2);
    line.contains(s3)
}

/// 盤上の駒を考慮しない飛車の利き
#[inline]
pub fn rook_step_effect(sq: Square) -> Bitboard {
    rook_effect(sq, Bitboard::EMPTY)
}

/// 盤上の駒を考慮しない角の利き
#[inline]
pub fn bishop_step_effect(sq: Square) -> Bitboard {
    bishop_effect(sq, Bitboard::EMPTY)
}

/// 盤上の駒を考慮しない香の利き
#[inline]
pub fn lance_step_effect(us: Color, sq: Square) -> Bitboard {
    lance_effect(us, sq, Bitboard::EMPTY)
}

/// 斜め1ステップの利き
#[inline]
pub fn cross45_step_effect(sq: Square) -> Bitboard {
    bishop_step_effect(sq) & king_effect(sq)
}

/// 盤上の駒を考慮しない女王の利き
#[inline]
pub fn queen_step_effect(sq: Square) -> Bitboard {
    rook_step_effect(sq) | bishop_step_effect(sq)
}

/// 敵陣（成りが可能な段）
#[inline]
fn enemy_field(us: Color) -> Bitboard {
    match us {
        Color::Black => RANK_BB[0] | RANK_BB[1] | RANK_BB[2],
        Color::White => RANK_BB[6] | RANK_BB[7] | RANK_BB[8],
    }
}

/// 1手詰め判定（簡易版）
///
/// 王手がかかっていない局面で1手詰めかどうかを判定する。
/// 高速化のためのテーブルは利用するが、判定は合法手全探索で行う。
pub fn mate_1ply(pos: &mut Position) -> Option<Move> {
    // 王手がかかっている局面では判定しない
    if pos.in_check() {
        return None;
    }

    let us = pos.side_to_move();
    if let Some(mv) = drop_mate::check_drop_mate(pos, us) {
        return Some(mv);
    }

    if let Some(mv) = move_mate::check_move_mate(pos, us) {
        return Some(mv);
    }

    brute_mate(pos)
}

/// フォールバックの全合法探索版1手詰め
fn brute_mate(pos: &mut Position) -> Option<Move> {
    let mut list = MoveList::new();
    movegen::generate_legal(pos, &mut list);

    for mv in list.iter() {
        let gives_check = pos.gives_check(*mv);
        pos.do_move(*mv, gives_check);

        let mut reply = MoveList::new();
        movegen::generate_legal(pos, &mut reply);
        let mate = reply.is_empty();

        pos.undo_move(*mv);

        if mate {
            return Some(*mv);
        }
    }

    None
}

/// 1手詰め判定の初期化
///
/// CHECK_CAND_BB、CHECK_AROUND_BB、NEXT_SQUAREテーブルを初期化する。
/// この関数は起動時に一度だけ呼ばれる。
pub fn init() {
    // LazyLockを使用するため、最初のアクセス時に自動的に初期化される
    let _ = &*tables::CHECK_CAND_BB;
    let _ = &*tables::CHECK_AROUND_BB;
    let _ = &*tables::NEXT_SQUARE;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_structure() {
        // モジュールが正しく構成されているかの基本テスト
        init();
    }

    #[test]
    fn test_aligned() {
        let s1 = Square::SQ_55;
        let s2 = Square::new(crate::types::File::File5, crate::types::Rank::Rank1);
        let s3 = Square::new(crate::types::File::File5, crate::types::Rank::Rank9);
        assert!(aligned(s1, s2, s3));
        let other = Square::new(crate::types::File::File4, crate::types::Rank::Rank4);
        assert!(!aligned(s1, s2, other));
    }

    #[test]
    fn test_can_promote() {
        let from = Square::new(crate::types::File::File5, crate::types::Rank::Rank3);
        let to = Square::new(crate::types::File::File5, crate::types::Rank::Rank4);
        assert!(can_promote(Color::Black, from, to));
        assert!(can_promote_to(Color::White, from.inverse()));
    }

    fn mate_by_brute(sfen: &str) -> Option<Move> {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        super::brute_mate(&mut pos)
    }

    fn mate_by_new(sfen: &str) -> Option<Move> {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        super::mate_1ply(&mut pos)
    }

    #[test]
    fn test_hirate_no_mate() {
        let sfen = crate::position::SFEN_HIRATE;
        assert_eq!(mate_by_new(sfen), None);
        assert_eq!(mate_by_brute(sfen), None);
    }

    #[test]
    fn test_drop_mate_gold_corner() {
        // 白玉1一、先手玉5九、先手: 飛3二・歩2一、持ち駒: 金
        // 1二に金打ちで詰み
        let sfen = "4K4/9/9/9/9/9/9/6R2/7Pk b G 1";
        let new_mv = mate_by_new(sfen);
        let brute_mv = mate_by_brute(sfen);
        assert_eq!(new_mv, brute_mv);
        assert!(new_mv.is_some());
    }
}
