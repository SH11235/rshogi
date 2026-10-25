//! 指し手生成を使わない合法手 ([`legal_moves_by_rules`]) による詰み手順の検証
//!
//! 玉の無い側がある局面では、指し手生成と `is_legal` を正解に使えない
//! (理由は `movegen::generate_legal_allowing_missing_kings`)。

use crate::bitboard::between_bb;
pub(super) use crate::movegen::legal_moves_by_rules;
use crate::movegen::{MoveList, generate_legal_all};
use crate::position::Position;
use crate::types::Move;

/// 詰み手順を [`legal_moves_by_rules`] で検証する: 全手が合法、攻め方の手はすべて王手、
/// 同じ局面に戻らない、最後の局面は王手されていて合法手が無い。`root` は変えない。
#[cfg(test)]
pub(super) fn is_valid_mate_pv_by_rules(root: &Position, pv: &[Move]) -> bool {
    check_mate_pv_by_rules(root, pv, false)
}

/// [`is_valid_mate_pv_by_rules`] の検証。`futile_end` なら、最後の局面の合法手が王手している駒に取られる
/// 合駒 (無駄合いの候補) だけでもよい (取った後に詰むことは確かめない)。
pub(super) fn check_mate_pv_by_rules(root: &Position, pv: &[Move], futile_end: bool) -> bool {
    if pv.is_empty() || pv.len().is_multiple_of(2) {
        return false;
    }
    let mut pos = root.clone();
    pos.clear_checkers_without_king();
    let mut keys = vec![pos.key()];
    for (i, &mv) in pv.iter().enumerate() {
        let legal = legal_moves_by_rules(&mut pos);
        let Some(&mv) = legal.iter().find(|m| m.raw() == mv.raw()) else {
            return false;
        };
        pos.do_move_with_missing_kings(mv);
        if i % 2 == 0 && !pos.in_check() {
            return false;
        }
        if keys.contains(&pos.key()) {
            return false;
        }
        keys.push(pos.key());
    }
    if !pos.in_check() {
        return false;
    }
    let replies = legal_moves_by_rules(&mut pos);
    replies.is_empty()
        || futile_end && replies.iter().all(|&mv| is_capturable_interposition(&mut pos, mv, false))
}

/// 王手されている局面 `pos` の応手 `mv` が合駒 (駒打ち) で、打った後に王手している駒がそれを取れるか。
/// `with_kings` なら両方の玉がある局面で、指し手生成で合法手を求める (そうでなければ [`legal_moves_by_rules`])。
pub(super) fn is_capturable_interposition(pos: &mut Position, mv: Move, with_kings: bool) -> bool {
    let checkers = pos.checkers();
    if !mv.is_drop() || checkers.count() != 1 {
        return false;
    }
    let checker = checkers.lsb().expect("王手している駒は 1 枚");
    if !between_bb(pos.king_square(pos.side_to_move()), checker).contains(mv.to()) {
        return false;
    }
    let is_capture = |m: &Move| !m.is_drop() && m.from() == checker && m.to() == mv.to();
    let capturable = if with_kings {
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
        let mut legal = MoveList::new();
        generate_legal_all(pos, &mut legal);
        legal.iter().any(is_capture)
    } else {
        pos.do_move_with_missing_kings(mv);
        legal_moves_by_rules(pos).iter().any(is_capture)
    };
    pos.undo_move(mv);
    capturable
}
