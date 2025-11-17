use crate::movegen::MoveGenerator;
use crate::shogi::{board::Bitboard, Color, Move, Piece, PieceType, Position, Square};
use smallvec::SmallVec;

#[derive(Clone, Copy, Debug)]
pub enum RootThreat {
    OppXsee { piece: PieceType, loss: i32 },
    PawnDropHead { piece: PieceType },
}

pub fn detect_major_threat(pos: &Position, us: Color, threshold: i32) -> Option<RootThreat> {
    let mut major_targets: Vec<(Square, PieceType)> = Vec::new();
    let mut friendly = pos.board.occupied_bb[us as usize];
    while let Some(sq) = friendly.pop_lsb() {
        let Some(piece) = pos.board.piece_on(sq) else {
            continue;
        };
        if !is_major(piece) {
            continue;
        }
        if let Some(loss) = worst_capture_loss(pos, sq, us) {
            if loss <= -threshold {
                return Some(RootThreat::OppXsee {
                    piece: piece.piece_type,
                    loss,
                });
            }
        }
        if pawn_drop_head_threat(pos, sq, us) {
            return Some(RootThreat::PawnDropHead {
                piece: piece.piece_type,
            });
        }
        major_targets.push((sq, piece.piece_type));
    }
    if let Some((piece, loss)) =
        detect_shortest_attack_after_enemy_move(pos, us, threshold, &major_targets)
    {
        return Some(RootThreat::OppXsee { piece, loss });
    }
    None
}

fn is_major(piece: Piece) -> bool {
    matches!(piece.piece_type, PieceType::Rook | PieceType::Bishop | PieceType::Gold)
        || (piece.piece_type == PieceType::Pawn && piece.promoted)
}

fn worst_capture_loss(pos: &Position, target: Square, us: Color) -> Option<i32> {
    let enemy = us.opposite();
    let mut attackers = pos.get_attackers_to(target, enemy);
    if attackers.is_empty() {
        return None;
    }
    let mut worst: Option<i32> = None;
    while let Some(from) = attackers.pop_lsb() {
        if let Some(piece) = pos.board.piece_on(from) {
            let mut consider = Vec::with_capacity(2);
            consider.push(Move::normal(from, target, false));
            if piece.piece_type.can_promote() && can_promote(piece.color, from, target) {
                consider.push(Move::normal(from, target, true));
            }
            for mv in consider {
                if !pos.is_legal_move(mv) {
                    continue;
                }
                let gain = pos.see(mv);
                let loss = -gain;
                if loss < worst.unwrap_or(0) {
                    worst = Some(loss);
                }
            }
        }
    }
    worst
}

fn pawn_drop_head_threat(pos: &Position, target: Square, us: Color) -> bool {
    let enemy = us.opposite();
    let Some(head) = head_square(target, us) else {
        return false;
    };
    if pos.board.piece_on(head).is_some() {
        return false;
    }
    let Some(hand_idx) = PieceType::Pawn.hand_index() else {
        return false;
    };
    if pos.hands[enemy as usize][hand_idx] == 0 {
        return false;
    }
    let mut blockers = Bitboard::EMPTY;
    blockers.set(target);
    blockers |= pos.board.occupied_bb[us as usize];
    let attacks = pos.get_attackers_to(head, enemy) & blockers;
    attacks != Bitboard::EMPTY
}

fn detect_shortest_attack_after_enemy_move(
    pos: &Position,
    us: Color,
    threshold: i32,
    targets: &[(Square, PieceType)],
) -> Option<(PieceType, i32)> {
    if targets.is_empty() {
        return None;
    }
    let enemy = us.opposite();
    let generator = MoveGenerator::new();
    let Ok(moves) = generator.generate_all(pos) else {
        return None;
    };
    for &mv in moves.as_slice() {
        if !pos.is_legal_move(mv) {
            continue;
        }
        let mut child = pos.clone();
        let undo = child.do_move(mv);
        for &(sq, piece) in targets {
            if let Some(loss) = worst_capture_loss(&child, sq, us) {
                if loss <= -threshold {
                    child.undo_move(mv, undo);
                    return Some((piece, loss));
                }
            }
            if pawn_drop_head_threat(&child, sq, us) {
                child.undo_move(mv, undo);
                return Some((piece, 0));
            }
            if let Some(loss) = evaluate_attackers_loss(&mut child, sq, enemy, threshold) {
                child.undo_move(mv, undo);
                return Some((piece, loss));
            }
        }
        child.undo_move(mv, undo);
    }
    None
}

fn evaluate_attackers_loss(
    child: &mut Position,
    target: Square,
    enemy: Color,
    threshold: i32,
) -> Option<i32> {
    let prev = child.side_to_move;
    child.side_to_move = enemy;
    let mut attackers = child.get_attackers_to(target, enemy);
    while let Some(from) = attackers.pop_lsb() {
        let Some(piece) = child.board.piece_on(from) else {
            continue;
        };
        for promote in promotion_options(piece, enemy, from, target) {
            let mv = Move::normal(from, target, promote);
            if !child.is_legal_move(mv) {
                continue;
            }
            let gain = child.see(mv);
            let loss = -gain;
            if loss <= -threshold {
                child.side_to_move = prev;
                return Some(loss);
            }
        }
    }
    child.side_to_move = prev;
    None
}

fn promotion_options(piece: Piece, color: Color, from: Square, to: Square) -> SmallVec<[bool; 2]> {
    let mut opts = SmallVec::<[bool; 2]>::new();
    if piece.promoted || !piece.piece_type.can_promote() {
        opts.push(false);
        return opts;
    }
    if must_promote(color, piece.piece_type, to) {
        opts.push(true);
        return opts;
    }
    if can_promote(color, from, to) {
        opts.push(true);
    }
    opts.push(false);
    opts
}

fn must_promote(color: Color, piece_type: PieceType, to: Square) -> bool {
    match (color, piece_type) {
        (Color::Black, PieceType::Pawn | PieceType::Lance) => to.rank() == 0,
        (Color::White, PieceType::Pawn | PieceType::Lance) => to.rank() == 8,
        (Color::Black, PieceType::Knight) => to.rank() <= 1,
        (Color::White, PieceType::Knight) => to.rank() >= 7,
        _ => false,
    }
}

fn can_promote(color: Color, from: Square, to: Square) -> bool {
    match color {
        Color::Black => from.rank() <= 2 || to.rank() <= 2,
        Color::White => from.rank() >= 6 || to.rank() >= 6,
    }
}

fn head_square(sq: Square, owner: Color) -> Option<Square> {
    match owner {
        Color::Black => {
            if sq.rank() == 0 {
                None
            } else {
                Some(Square::new(sq.file(), sq.rank() - 1))
            }
        }
        Color::White => {
            if sq.rank() == 8 {
                None
            } else {
                Some(Square::new(sq.file(), sq.rank() + 1))
            }
        }
    }
}
