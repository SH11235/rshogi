//! 1 手詰めの利き問い合わせ。MODE は入口で選び、問い合わせごとの実行時切替を避ける。

use crate::bitboard::{
    Bitboard, gold_effect, king_effect, knight_effect, pawn_effect, silver_effect,
};
use crate::position::Position;
use crate::types::{Color, PieceType, Square};

use super::tables::check_around_bb;

pub(super) struct AttackQuery<const MODE: u8> {
    near: Option<NearAttacks>,
}

impl<const MODE: u8> AttackQuery<MODE> {
    pub(super) fn new() -> Self {
        Self { near: None }
    }

    #[inline(always)]
    pub(super) fn attacked(
        &mut self,
        pos: &Position,
        us: Color,
        sq: Square,
        occupied: Bitboard,
        from: Option<Square>,
    ) -> bool {
        let excluded = from.map_or(Bitboard::EMPTY, Bitboard::from_square);
        if MODE == 0 {
            return (pos.attackers_to_color_occ(us, sq, occupied) & !excluded).is_not_empty();
        }
        if MODE == 1 {
            return pos.is_attacked_by_excluding(us, sq, occupied, excluded);
        }
        self.attacked_near(pos, us, sq, occupied, from)
    }

    // マップ生成・問い合わせの本体を多数の候補判定箇所へ展開しない。
    #[inline(never)]
    fn attacked_near(
        &mut self,
        pos: &Position,
        us: Color,
        sq: Square,
        occupied: Bitboard,
        from: Option<Square>,
    ) -> bool {
        debug_assert!(king_effect(pos.king_square(!us)).contains(sq));
        let near = self.near.get_or_insert_with(|| NearAttacks::new(pos, us));
        let step = if let Some(from) = from {
            near.twice | (near.once & !step_effect(us, pos.piece_on(from).piece_type(), from))
        } else {
            near.once
        };
        step.contains(sq)
            || pos.is_attacked_by_sliders(
                us,
                sq,
                occupied,
                from.map_or(Bitboard::EMPTY, Bitboard::from_square),
            )
    }
}

struct NearAttacks {
    once: Bitboard,
    twice: Bitboard,
}

impl NearAttacks {
    fn new(pos: &Position, us: Color) -> Self {
        let king = pos.king_square(!us);
        let neighbors = king_effect(king);
        let mut result = Self {
            once: Bitboard::EMPTY,
            twice: Bitboard::EMPTY,
        };
        for pt in [
            PieceType::Pawn,
            PieceType::Knight,
            PieceType::Silver,
            PieceType::Gold,
            PieceType::King,
        ] {
            let pieces = match pt {
                PieceType::Gold => pos.golds_c(us),
                PieceType::King => {
                    pos.pieces(us, PieceType::King)
                        | pos.pieces(us, PieceType::Horse)
                        | pos.pieces(us, PieceType::Dragon)
                }
                _ => pos.pieces(us, pt),
            } & check_around_bb(us, pt, king);
            for from in pieces.iter() {
                let attacks = step_effect(us, pt, from) & neighbors;
                result.twice |= result.once & attacks;
                result.once |= attacks;
            }
        }
        result
    }
}

fn step_effect(us: Color, pt: PieceType, from: Square) -> Bitboard {
    match pt {
        PieceType::Pawn => pawn_effect(us, from),
        PieceType::Knight => knight_effect(us, from),
        PieceType::Silver => silver_effect(us, from),
        PieceType::Gold
        | PieceType::ProPawn
        | PieceType::ProLance
        | PieceType::ProKnight
        | PieceType::ProSilver => gold_effect(us, from),
        PieceType::King | PieceType::Horse | PieceType::Dragon => king_effect(from),
        _ => Bitboard::EMPTY,
    }
}
