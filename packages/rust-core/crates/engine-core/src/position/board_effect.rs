use crate::bitboard::Direct;
use crate::bitboard::{
    bishop_effect, direct_effect, dragon_effect, gold_effect, horse_effect, king_effect,
    knight_effect, lance_effect, pawn_effect, rook_effect, silver_effect, Bitboard,
};
use crate::types::{Color, Piece, PieceType, Square};

use super::Position;

const DIRECTS: [Direct; 8] = [
    Direct::RU,
    Direct::R,
    Direct::RD,
    Direct::U,
    Direct::D,
    Direct::LU,
    Direct::L,
    Direct::LD,
];

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BoardEffects {
    counts: [[u8; Square::NUM]; Color::NUM],
}

impl BoardEffects {
    pub(crate) fn new() -> Self {
        BoardEffects {
            counts: [[0u8; Square::NUM]; Color::NUM],
        }
    }

    #[inline]
    pub(crate) fn effect(&self, color: Color, sq: Square) -> u8 {
        self.counts[color.index()][sq.index()]
    }

    fn add_delta(&mut self, color: Color, sq: Square, delta: i8) {
        let current = self.counts[color.index()][sq.index()] as i16;
        let next = current + delta as i16;
        debug_assert!(
            (0..=u8::MAX as i16).contains(&next),
            "board_effect overflow/underflow: color={:?}, sq={:?}, current={current}, delta={delta}",
            color,
            sq
        );
        self.counts[color.index()][sq.index()] = next as u8;
    }

    fn apply_bitboard(&mut self, color: Color, bb: Bitboard, delta: i8) {
        if delta == 0 {
            return;
        }
        for sq in bb.iter() {
            self.add_delta(color, sq, delta);
        }
    }
}

pub(crate) fn compute_board_effects(pos: &Position) -> BoardEffects {
    let mut effects = BoardEffects::new();
    let occupied = pos.occupied();

    for color in [Color::Black, Color::White] {
        let bb = pos.pieces_c(color);
        for sq in bb.iter() {
            let pc = pos.piece_on(sq);
            let effect_bb = attacks_from(pc, sq, occupied);
            effects.apply_bitboard(color, effect_bb, 1);
        }
    }

    effects
}

pub(crate) fn add_piece_effect(
    effects: &mut BoardEffects,
    pc: Piece,
    sq: Square,
    occupied: Bitboard,
    delta: i8,
) {
    let bb = attacks_from(pc, sq, occupied);
    effects.apply_bitboard(pc.color(), bb, delta);
}

pub(crate) fn update_xray_for_square(
    board: &[Piece; Square::NUM],
    effects: &mut BoardEffects,
    sq: Square,
    occupied_before: Bitboard,
    occupied_after: Bitboard,
    skip_moved: Option<Square>,
    skip_captured: Option<Square>,
) {
    let before_occ = occupied_before.contains(sq);
    let after_occ = occupied_after.contains(sq);
    if before_occ == after_occ {
        return;
    }

    let (delta, ray_occupied) = if before_occ {
        (1, occupied_after) // occupied -> empty
    } else {
        (-1, occupied_before) // empty -> occupied
    };

    for dir in DIRECTS {
        let Some(blocker_sq) = first_piece_in_direction(occupied_before, sq, dir) else {
            continue;
        };
        if Some(blocker_sq) == skip_moved || Some(blocker_sq) == skip_captured {
            continue;
        }

        let pc = board[blocker_sq.index()];
        if pc.is_none() {
            continue;
        }

        let opposite = opposite_dir(dir);
        if !piece_attacks_dir(pc, opposite) {
            continue;
        }

        let ray = direct_effect(sq, opposite, ray_occupied);
        effects.apply_bitboard(pc.color(), ray, delta);
    }
}

fn attacks_from(pc: Piece, sq: Square, occupied: Bitboard) -> Bitboard {
    let color = pc.color();
    match pc.piece_type() {
        PieceType::Pawn => pawn_effect(color, sq),
        PieceType::Lance => lance_effect(color, sq, occupied),
        PieceType::Knight => knight_effect(color, sq),
        PieceType::Silver => silver_effect(color, sq),
        PieceType::Gold
        | PieceType::ProPawn
        | PieceType::ProLance
        | PieceType::ProKnight
        | PieceType::ProSilver => gold_effect(color, sq),
        PieceType::Bishop => bishop_effect(sq, occupied),
        PieceType::Rook => rook_effect(sq, occupied),
        PieceType::Horse => horse_effect(sq, occupied),
        PieceType::Dragon => dragon_effect(sq, occupied),
        PieceType::King => king_effect(sq),
    }
}

fn piece_attacks_dir(pc: Piece, dir: Direct) -> bool {
    match pc.piece_type() {
        PieceType::Lance => match pc.color() {
            Color::Black => matches!(dir, Direct::U),
            Color::White => matches!(dir, Direct::D),
        },
        PieceType::Bishop | PieceType::Horse => {
            matches!(dir, Direct::RU | Direct::RD | Direct::LU | Direct::LD)
        }
        PieceType::Rook | PieceType::Dragon => {
            matches!(dir, Direct::U | Direct::D | Direct::L | Direct::R)
        }
        _ => false,
    }
}

fn opposite_dir(dir: Direct) -> Direct {
    match dir {
        Direct::RU => Direct::LD,
        Direct::R => Direct::L,
        Direct::RD => Direct::LU,
        Direct::U => Direct::D,
        Direct::D => Direct::U,
        Direct::LU => Direct::RD,
        Direct::L => Direct::R,
        Direct::LD => Direct::RU,
    }
}

fn direct_delta(dir: Direct) -> i8 {
    match dir {
        Direct::RU => Square::DELTA_RU,
        Direct::R => Square::DELTA_R,
        Direct::RD => Square::DELTA_RD,
        Direct::U => Square::DELTA_U,
        Direct::D => Square::DELTA_D,
        Direct::LU => Square::DELTA_LU,
        Direct::L => Square::DELTA_L,
        Direct::LD => Square::DELTA_LD,
    }
}

fn first_piece_in_direction(occupied: Bitboard, start: Square, dir: Direct) -> Option<Square> {
    let delta = direct_delta(dir);
    let mut cur = start;
    while let Some(next) = cur.offset(delta) {
        if occupied.contains(next) {
            return Some(next);
        }
        cur = next;
    }
    None
}
