//! 玉の無い側がある局面の合法手
//!
//! 玉の無い側がある局面では、`Position` は玉の無い側の `king_square` (既定の升) を玉とみなして
//! 王手・ピン・打ち歩詰めを判定するので、指し手生成と `is_legal` を正解に使えない。ここでは移動元・
//! 移動先・成りと打つ駒を総当たりし、駒の動き (`pseudo_legal`) と指した後の玉への利きから合法手を決める。

use crate::position::Position;
use crate::types::{Color, Move, PieceType, Square};

use super::{MoveList, generate_legal_all_with_pass};

/// 局面の合法手 (不成とパスを含む)。玉の無い側がある局面も正しく扱う。
///
/// 両方の玉がある局面は [`generate_legal_all_with_pass`] と同じ手を返す。玉の無い側がある局面では、
/// 玉の無い側の手は自玉の安全を問わず、打ち歩詰めは相手玉があるときだけ禁じ、玉を取る手は認めない。
/// 総当たりで判定するので、両方の玉がある局面の生成より遅い。探索では使わないこと。
///
/// 手番側の玉が無い局面の `checkers` (`set_sfen` は既定の升への利きを王手として記録する) は見ない。
pub fn generate_legal_allowing_missing_kings(pos: &Position, list: &mut MoveList) {
    if has_both_kings(pos) {
        generate_legal_all_with_pass(pos, list);
        return;
    }
    let mut pos = pos.clone();
    pos.clear_checkers_without_king();
    for mv in legal_moves_by_rules(&mut pos) {
        list.push(mv);
    }
    if pos.can_pass() {
        list.push(Move::PASS);
    }
}

fn has_both_kings(pos: &Position) -> bool {
    [Color::Black, Color::White]
        .into_iter()
        .all(|color| !pos.pieces(color, PieceType::King).is_empty())
}

/// 局面の合法手 (不成を含み、パスを除く)。規則は [`generate_legal_allowing_missing_kings`] と同じ。
///
/// 前提: 手番側が王手されていれば `checkers` がその王手を表し、玉が無ければ空。
/// 局面は呼び出し前の状態に戻して返る。
pub(crate) fn legal_moves_by_rules(pos: &mut Position) -> Vec<Move> {
    let us = pos.side_to_move();
    let mut candidates = Vec::new();
    for from in pos.pieces_c(us).iter() {
        for to in Square::all() {
            candidates.push(Move::new_move(from, to, false));
            candidates.push(Move::new_move(from, to, true));
        }
    }
    for pt in PieceType::HAND_PIECES {
        if pos.hand(us).has(pt) {
            candidates.extend(Square::all().map(|to| Move::new_drop(pt, to)));
        }
    }
    let mut legal = Vec::new();
    for mv in candidates {
        if let Some(mv) = pos.to_move(mv)
            && is_legal_by_rules(pos, mv)
        {
            legal.push(mv);
        }
    }
    legal
}

fn is_legal_by_rules(pos: &mut Position, mv: Move) -> bool {
    if !pos.pseudo_legal(mv) {
        return false;
    }
    let captured = if mv.is_drop() {
        None
    } else {
        Some(pos.piece_on(mv.to()))
    };
    if captured.is_some_and(|pc| pc.is_some() && pc.piece_type() == PieceType::King) {
        return false;
    }
    let us = pos.side_to_move();
    // 王手を盤上の利きから数え直す (玉の無い側への王手は記録しない)
    pos.do_move_with_missing_kings(mv);
    let king = pos.pieces(us, PieceType::King);
    let mut legal = king.is_empty() || pos.attackers_to_c(pos.king_square(us), !us).is_empty();
    if legal && mv.is_drop() && mv.drop_piece_type() == PieceType::Pawn && pos.in_check() {
        // 打ち歩詰め
        legal = !legal_moves_by_rules(pos).is_empty();
    }
    pos.undo_move(mv);
    legal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legal_usi(sfen: &str) -> Vec<String> {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        let mut list = MoveList::new();
        generate_legal_allowing_missing_kings(&pos, &mut list);
        let mut moves: Vec<String> = list.iter().map(|m| m.to_usi()).collect();
        moves.sort();
        moves
    }

    /// 受け方の玉が 2一 にあると、`set_sfen` は既定の升 1一 への利きを攻め方への王手として記録する
    #[test]
    fn attacker_without_king_is_not_in_check_from_default_square() {
        let moves = legal_usi("7k1/9/9/9/9/9/9/9/9 b G 1");
        assert!(moves.contains(&"G*5e".to_string()));
        assert!(moves.contains(&"G*2b".to_string()));
        assert_eq!(moves.len(), 80);
    }

    /// 攻め方の駒が既定の升 1一 にあっても、玉の移動として扱わない
    #[test]
    fn attacker_piece_on_default_square_moves_freely() {
        let moves = legal_usi("4k3G/9/9/9/9/9/9/9/9 b - 1");
        assert!(moves.contains(&"1a2a".to_string()));
        assert!(moves.contains(&"1a1b".to_string()));
    }

    /// 攻め方 (後手) の玉が無いとき、受け方の 1二 歩合いを打ち歩詰めとして消さない
    #[test]
    fn defender_pawn_interposition_toward_default_square_is_legal() {
        let mut pos = Position::new();
        pos.set_sfen("9/6GP1/7LK/7GL/9/9/9/9/9 w Pl 1").unwrap();
        let drop = Move::from_usi("L*1a").unwrap();
        let mut list = MoveList::new();
        generate_legal_allowing_missing_kings(&pos, &mut list);
        let drop = list.iter().copied().find(|m| m.raw() == drop.raw()).unwrap();
        pos.do_move_with_missing_kings(drop);
        let mut replies = MoveList::new();
        generate_legal_allowing_missing_kings(&pos, &mut replies);
        let replies: Vec<String> = replies.iter().map(|m| m.to_usi()).collect();
        assert_eq!(replies, vec!["P*1b".to_string()]);
    }

    /// 相手玉の無い打ち歩は打ち歩詰めにならない。相手玉を取る手は認めない
    #[test]
    fn pawn_drop_mate_and_king_capture() {
        // 攻め方の玉が無い局面の打ち歩詰め (1二歩 は 2三金 が支え、2一 は 3二銀 が押さえる)
        let moves = legal_usi("8k/6S2/7G1/9/9/9/9/9/9 b P 1");
        assert!(!moves.contains(&"P*1b".to_string()));
        assert!(moves.contains(&"P*1c".to_string()));
        // 受け方の玉を取る手は無い
        let moves = legal_usi("4k4/4G4/9/9/9/9/9/9/9 b - 1");
        assert!(!moves.contains(&"5b5a".to_string()));
    }

    #[test]
    fn pass_is_listed_when_rights_remain() {
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/9 b G 1").unwrap();
        pos.enable_pass_rights(1, 1);
        let mut list = MoveList::new();
        generate_legal_allowing_missing_kings(&pos, &mut list);
        assert!(list.iter().any(|m| m.is_pass()));
    }
}
