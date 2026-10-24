//! 指し手生成を使わない合法手 ([`legal_moves_by_rules`]) による詰み手順の検証
//!
//! 玉の無い側がある局面では、指し手生成と `is_legal` を正解に使えない
//! (理由は `movegen::generate_legal_allowing_missing_kings`)。

pub(super) use crate::movegen::legal_moves_by_rules;
use crate::position::Position;
use crate::types::Move;

/// 詰み手順を [`legal_moves_by_rules`] で検証する: 全手が合法、攻め方の手はすべて王手、
/// 同じ局面に戻らない、最後の局面は王手されていて合法手が無い。`root` は変えない。
pub(super) fn is_valid_mate_pv_by_rules(root: &Position, pv: &[Move]) -> bool {
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
    pos.in_check() && legal_moves_by_rules(&mut pos).is_empty()
}
