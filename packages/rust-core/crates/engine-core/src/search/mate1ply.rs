use crate::movegen::MoveGenerator;
use crate::shogi::{Color, Move, Position};

#[inline(always)]
fn is_mate_after_move(pos: &mut Position, reply_gen: &MoveGenerator) -> bool {
    let mut in_check = pos.is_in_check();
    if !in_check {
        in_check = pos.is_in_check_slow();
    }
    if !in_check {
        return false;
    }
    matches!(reply_gen.has_legal_moves(pos), Ok(false))
}

fn mate_in_one_impl(pos: &mut Position) -> Option<Move> {
    let generator = MoveGenerator::new();
    let reply_checker = MoveGenerator::new();
    let Ok(moves) = generator.generate_all(pos) else {
        return None;
    };
    for &mv in moves.as_slice() {
        let undo = pos.do_move(mv);
        let mate = is_mate_after_move(pos, &reply_checker);
        pos.undo_move(mv, undo);
        if mate {
            return Some(mv);
        }
    }
    None
}

/// Returns a mate-in-one move for `side` if available.
pub fn mate_in_one_for_side(pos: &mut Position, side: Color) -> Option<Move> {
    if pos.side_to_move != side {
        debug_assert_eq!(
            pos.side_to_move, side,
            "mate_in_one_for_side expects pos.side_to_move == side"
        );
        return None;
    }
    mate_in_one_impl(pos)
}

/// Detects opponent mate-in-one after playing `mv`.
pub fn enemy_mate_in_one_after(pos: &mut Position, mv: Move) -> Option<Move> {
    let undo = pos.do_move(mv);
    let enemy = pos.side_to_move;
    let res = mate_in_one_for_side(pos, enemy);
    pos.undo_move(mv, undo);
    res
}
