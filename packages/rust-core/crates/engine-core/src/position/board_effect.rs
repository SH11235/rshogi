use crate::bitboard::{
    bishop_effect, direct_effect, direct_of, dragon_effect, gold_effect, horse_effect, king_effect,
    knight_effect, lance_effect, pawn_effect, rook_effect, silver_effect, Bitboard, Direct,
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

const BISHOP_DIR: u8 = (1u8 << Direct::RU as u8)
    | (1u8 << Direct::RD as u8)
    | (1u8 << Direct::LU as u8)
    | (1u8 << Direct::LD as u8);
const ROOK_DIR: u8 = (1u8 << Direct::R as u8)
    | (1u8 << Direct::L as u8)
    | (1u8 << Direct::U as u8)
    | (1u8 << Direct::D as u8);

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
        // 利き数は最大でも片側20枚分でu8に収まる前提のため、リリースではキャストのみ。
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

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LongEffects {
    dirs: [u16; Square::NUM],
}

impl LongEffects {
    pub(crate) fn new() -> Self {
        LongEffects {
            dirs: [0u16; Square::NUM],
        }
    }

    #[inline]
    pub(crate) fn long_effect16(&self, sq: Square) -> u16 {
        self.dirs[sq.index()]
    }

    #[inline]
    fn toggle(&mut self, sq: Square, value: u16) {
        self.dirs[sq.index()] ^= value;
    }
}

pub(crate) fn compute_board_effects_and_long_effects(
    pos: &Position,
) -> (BoardEffects, LongEffects) {
    let mut effects = BoardEffects::new();
    let mut long_effects = LongEffects::new();
    let occupied = pos.occupied();

    for color in [Color::Black, Color::White] {
        let bb = pos.pieces_c(color);
        for sq in bb.iter() {
            let pc = pos.piece_on(sq);
            let effect_bb = attacks_from(pc, sq, occupied);
            effects.apply_bitboard(color, effect_bb, 1);

            if has_long_effect(pc) {
                // 長い利きは馬/龍の追加1マス利きを含めない（YaneuraOuのlong_effect定義に合わせる）。
                let long_pc = pc.unpromote();
                let long_bb = attacks_from(long_pc, sq, occupied);
                let shift = if pc.color() == Color::Black { 0 } else { 8 };
                for to in long_bb.iter() {
                    let Some(dir) = direct_of(sq, to) else {
                        continue;
                    };
                    let bit = 1u16 << (dir as u8);
                    long_effects.toggle(to, bit << shift);
                }
            }
        }
    }

    (effects, long_effects)
}

pub(crate) fn update_by_dropping_piece(
    effects: &mut BoardEffects,
    long_effects: &mut LongEffects,
    occupied: Bitboard,
    to: Square,
    dropped_pc: Piece,
) {
    let us = dropped_pc.color();
    let inc_target = short_effects_from(dropped_pc, to);
    effects.apply_bitboard(us, inc_target, 1);

    let dir_bw_us = long_effect16_of(dropped_pc);
    let dir_bw_others = long_effects.long_effect16(to);
    update_long_effect_from(effects, long_effects, occupied, to, dir_bw_us, dir_bw_others, 1, us);
}

pub(crate) fn update_by_no_capturing_piece(
    effects: &mut BoardEffects,
    long_effects: &mut LongEffects,
    occupied: Bitboard,
    from: Square,
    to: Square,
    moved_pc: Piece,
    moved_after_pc: Piece,
) {
    let us = moved_pc.color();
    let mut dec_target = short_effects_from(moved_pc, from);
    let mut inc_target = short_effects_from(moved_after_pc, to);

    let and_target = inc_target & dec_target;
    inc_target ^= and_target;
    dec_target ^= and_target;

    effects.apply_bitboard(us, inc_target, 1);
    effects.apply_bitboard(us, dec_target, -1);

    let dir_mask = dir_mask_for_move(from, to);
    let dir_bw_us = long_effect16_of(moved_pc) & dir_mask;
    let dir_bw_others = long_effects.long_effect16(from) & dir_mask;
    update_long_effect_from(
        effects,
        long_effects,
        occupied,
        from,
        dir_bw_us,
        dir_bw_others,
        -1,
        us,
    );

    let dir_bw_us = long_effect16_of(moved_after_pc);
    let dir_bw_others = long_effects.long_effect16(to);
    update_long_effect_from(effects, long_effects, occupied, to, dir_bw_us, dir_bw_others, 1, us);
}

pub(crate) fn update_by_capturing_piece(
    effects: &mut BoardEffects,
    long_effects: &mut LongEffects,
    occupied: Bitboard,
    from: Square,
    to: Square,
    moved_pc: Piece,
    moved_after_pc: Piece,
    captured_pc: Piece,
) {
    let us = moved_pc.color();
    let mut dec_target = short_effects_from(moved_pc, from);
    let mut inc_target = short_effects_from(moved_after_pc, to);

    let and_target = inc_target & dec_target;
    inc_target ^= and_target;
    dec_target ^= and_target;

    effects.apply_bitboard(us, inc_target, 1);
    effects.apply_bitboard(us, dec_target, -1);

    let dec_target = short_effects_from(captured_pc, to);
    effects.apply_bitboard(captured_pc.color(), dec_target, -1);

    let dir_mask = dir_mask_for_move(from, to);
    let dir_bw_us = long_effect16_of(moved_pc) & dir_mask;
    let dir_bw_others = long_effects.long_effect16(from) & dir_mask;
    update_long_effect_from(
        effects,
        long_effects,
        occupied,
        from,
        dir_bw_us,
        dir_bw_others,
        -1,
        us,
    );

    let dir_bw_us = long_effect16_of(moved_after_pc);
    let dir_bw_others = long_effect16_of(captured_pc);
    update_long_effect_from(effects, long_effects, occupied, to, dir_bw_us, dir_bw_others, 1, us);
}

pub(crate) fn rewind_by_dropping_piece(
    effects: &mut BoardEffects,
    long_effects: &mut LongEffects,
    occupied: Bitboard,
    to: Square,
    dropped_pc: Piece,
) {
    let us = dropped_pc.color();
    let dec_target = short_effects_from(dropped_pc, to);
    effects.apply_bitboard(us, dec_target, -1);

    let dir_bw_us = long_effect16_of(dropped_pc);
    let dir_bw_others = long_effects.long_effect16(to);
    update_long_effect_from(effects, long_effects, occupied, to, dir_bw_us, dir_bw_others, -1, us);
}

pub(crate) fn rewind_by_no_capturing_piece(
    effects: &mut BoardEffects,
    long_effects: &mut LongEffects,
    occupied: Bitboard,
    from: Square,
    to: Square,
    moved_pc: Piece,
    moved_after_pc: Piece,
) {
    let us = moved_pc.color();
    let mut inc_target = short_effects_from(moved_pc, from);
    let mut dec_target = short_effects_from(moved_after_pc, to);

    let and_target = inc_target & dec_target;
    inc_target ^= and_target;
    dec_target ^= and_target;

    effects.apply_bitboard(us, inc_target, 1);
    effects.apply_bitboard(us, dec_target, -1);

    let dir_bw_us = long_effect16_of(moved_after_pc);
    let dir_bw_others = long_effects.long_effect16(to);
    update_long_effect_from(effects, long_effects, occupied, to, dir_bw_us, dir_bw_others, -1, us);

    let dir_mask = dir_mask_for_move(from, to);
    let dir_bw_us = long_effect16_of(moved_pc) & dir_mask;
    let dir_bw_others = long_effects.long_effect16(from) & dir_mask;
    update_long_effect_from(effects, long_effects, occupied, from, dir_bw_us, dir_bw_others, 1, us);
}

pub(crate) fn rewind_by_capturing_piece(
    effects: &mut BoardEffects,
    long_effects: &mut LongEffects,
    occupied: Bitboard,
    from: Square,
    to: Square,
    moved_pc: Piece,
    moved_after_pc: Piece,
    captured_pc: Piece,
) {
    let us = moved_pc.color();
    let mut inc_target = short_effects_from(moved_pc, from);
    let mut dec_target = short_effects_from(moved_after_pc, to);

    let and_target = inc_target & dec_target;
    inc_target ^= and_target;
    dec_target ^= and_target;

    effects.apply_bitboard(us, inc_target, 1);
    effects.apply_bitboard(us, dec_target, -1);

    let inc_target = short_effects_from(captured_pc, to);
    effects.apply_bitboard(captured_pc.color(), inc_target, 1);

    let dir_bw_us = long_effect16_of(moved_after_pc);
    let dir_bw_others = long_effect16_of(captured_pc);
    update_long_effect_from(effects, long_effects, occupied, to, dir_bw_us, dir_bw_others, -1, us);

    let dir_mask = dir_mask_for_move(from, to);
    let dir_bw_us = long_effect16_of(moved_pc) & dir_mask;
    let dir_bw_others = long_effects.long_effect16(from) & dir_mask;
    update_long_effect_from(effects, long_effects, occupied, from, dir_bw_us, dir_bw_others, 1, us);
}

/// 長い利きの差分更新を行う。
///
/// - `dir_bw_us`: 自分の駒の長い利き方向（下位8bit: 先手、上位8bit: 後手）
/// - `dir_bw_others`: 相手の長い利き方向（同上）
/// - `p`: 増減量（1: 追加、-1: 削除）
///
/// ロジック:
/// 1. XORで変化した方向のみ抽出
/// 2. 方向ごとに味方/相手の利きを分離
/// 3. 直線上の升に対して利きカウントと長い利きを更新
fn update_long_effect_from(
    effects: &mut BoardEffects,
    long_effects: &mut LongEffects,
    occupied: Bitboard,
    from: Square,
    dir_bw_us: u16,
    dir_bw_others: u16,
    p: i8,
    us: Color,
) {
    let mut dir_bw = dir_bw_us ^ dir_bw_others;
    let us_is_black = us == Color::Black;

    while dir_bw != 0 {
        let lsb = dir_bw.trailing_zeros() as u8;
        let dir_index = (lsb & 7) as usize;
        let mut value = (1u16 << dir_index) | (1u16 << (dir_index + 8));
        value &= dir_bw;
        dir_bw &= !value;

        let same_color = if us_is_black {
            (value & 0x00ff) != 0
        } else {
            (value & 0xff00) != 0
        };
        let e1 = if (dir_bw_us & value) != 0 {
            p
        } else if same_color {
            -p
        } else {
            0
        };

        let other_color = if us_is_black {
            (value & 0xff00) != 0
        } else {
            (value & 0x00ff) != 0
        };
        let e2 = if other_color { -p } else { 0 };

        if e1 == 0 && e2 == 0 {
            continue;
        }

        let dir = DIRECTS[dir_index];
        let ray = direct_effect(from, dir, occupied);
        for sq in ray.iter() {
            long_effects.toggle(sq, value);
            if e1 != 0 {
                effects.add_delta(us, sq, e1);
            }
            if e2 != 0 {
                effects.add_delta(!us, sq, e2);
            }
        }
    }
}

fn short_effects_from(pc: Piece, sq: Square) -> Bitboard {
    let color = pc.color();
    match pc.piece_type() {
        PieceType::Pawn => pawn_effect(color, sq),
        PieceType::Knight => knight_effect(color, sq),
        PieceType::Silver => silver_effect(color, sq),
        PieceType::Gold
        | PieceType::ProPawn
        | PieceType::ProLance
        | PieceType::ProKnight
        | PieceType::ProSilver => gold_effect(color, sq),
        PieceType::Horse => orthogonal_step_effect(sq),
        PieceType::Dragon => diagonal_step_effect(sq),
        PieceType::King => king_effect(sq),
        _ => Bitboard::EMPTY,
    }
}

fn orthogonal_step_effect(sq: Square) -> Bitboard {
    let mut bb = Bitboard::EMPTY;
    if let Some(next) = sq.offset(Square::DELTA_U) {
        bb.set(next);
    }
    if let Some(next) = sq.offset(Square::DELTA_D) {
        bb.set(next);
    }
    if let Some(next) = sq.offset(Square::DELTA_L) {
        bb.set(next);
    }
    if let Some(next) = sq.offset(Square::DELTA_R) {
        bb.set(next);
    }
    bb
}

fn diagonal_step_effect(sq: Square) -> Bitboard {
    let mut bb = Bitboard::EMPTY;
    if let Some(next) = sq.offset(Square::DELTA_RU) {
        bb.set(next);
    }
    if let Some(next) = sq.offset(Square::DELTA_RD) {
        bb.set(next);
    }
    if let Some(next) = sq.offset(Square::DELTA_LU) {
        bb.set(next);
    }
    if let Some(next) = sq.offset(Square::DELTA_LD) {
        bb.set(next);
    }
    bb
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

fn has_long_effect(pc: Piece) -> bool {
    matches!(
        pc.piece_type(),
        PieceType::Lance
            | PieceType::Bishop
            | PieceType::Rook
            | PieceType::Horse
            | PieceType::Dragon
    )
}

fn long_effect8_of(pc: Piece) -> u8 {
    match pc.piece_type() {
        PieceType::Lance => match pc.color() {
            Color::Black => 1u8 << Direct::U as u8,
            Color::White => 1u8 << Direct::D as u8,
        },
        PieceType::Bishop | PieceType::Horse => BISHOP_DIR,
        PieceType::Rook | PieceType::Dragon => ROOK_DIR,
        _ => 0,
    }
}

fn long_effect16_of(pc: Piece) -> u16 {
    let dir8 = long_effect8_of(pc);
    if dir8 == 0 {
        return 0;
    }
    let shift = if pc.color() == Color::Black { 0 } else { 8 };
    (dir8 as u16) << shift
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

fn dir_mask_for_move(from: Square, to: Square) -> u16 {
    if let Some(dir) = direct_of(from, to) {
        let opposite = opposite_dir(dir);
        let bit = 1u16 << (opposite as u8);
        !(bit | (bit << 8))
    } else {
        0xffff
    }
}
