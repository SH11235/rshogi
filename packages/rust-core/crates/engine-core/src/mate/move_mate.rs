// 駒移動による1手詰め判定

use crate::movegen::{self, MoveList};
use crate::position::Position;
use crate::types::{Color, Move};

/// 駒移動による1手詰めを判定
///
/// # Arguments
/// * `pos` - 局面（手番側が us の前提）
/// * `us` - 攻撃側の色
///
/// # Returns
/// 1手詰めの手があれば`Some(Move)`、なければ`None`
pub fn check_move_mate(pos: &mut Position, us: Color) -> Option<Move> {
    debug_assert_eq!(pos.side_to_move(), us);

    let mut list = MoveList::new();
    movegen::generate_legal(pos, &mut list);

    for mv in list.iter() {
        if mv.is_drop() {
            continue;
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_mate_compile() {
        let _ = std::mem::size_of::<Option<Move>>();
    }
}
