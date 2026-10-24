//! 指し手生成を使わない合法手の判定と、それによる詰み手順の検証
//!
//! 玉の無い側がある局面では、`Position` は玉の無い側の `king_square` (既定の升) を玉とみなして
//! 王手・ピン・打ち歩詰めを判定するので、指し手生成と `is_legal` を正解に使えない。ここでは移動元・
//! 移動先・成りと打つ駒を総当たりし、駒の動き (`pseudo_legal`) と指した後の玉への利きから合法手を決める。

use crate::bitboard::Bitboard;
use crate::position::Position;
use crate::types::{Move, PieceType, Square};

/// 局面の合法手 (不成を含む)。玉の無い側の手は自玉の安全を問わず、打ち歩詰めは相手玉があるときだけ禁じる。
///
/// 前提: 手番側が王手されていれば `checkers` がその王手を表し、玉が無ければ空。
/// 局面は呼び出し前の状態に戻して返る。
pub(super) fn legal_moves_by_rules(pos: &mut Position) -> Vec<Move> {
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

/// 詰み手順を [`legal_moves_by_rules`] で検証する: 全手が合法、攻め方の手はすべて王手、
/// 同じ局面に戻らない、最後の局面は王手されていて合法手が無い。`root` は変えない。
pub(super) fn is_valid_mate_pv_by_rules(root: &Position, pv: &[Move]) -> bool {
    if pv.is_empty() || pv.len().is_multiple_of(2) {
        return false;
    }
    let mut pos = root.clone();
    if pos.pieces(pos.side_to_move(), PieceType::King).is_empty() {
        pos.state_mut().checkers = Bitboard::EMPTY;
    }
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
