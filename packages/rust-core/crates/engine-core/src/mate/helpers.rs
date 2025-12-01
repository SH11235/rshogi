// 1手詰め探索用のヘルパー関数

use crate::bitboard::{
    bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect, knight_effect,
    lance_effect, pawn_effect, rook_effect, silver_effect, Bitboard, FILE_BB,
};
use crate::position::Position;
use crate::types::{Color, PieceType, Square};

use super::aligned;

/// Sliderの利きを列挙する
///
/// # Arguments
/// * `pos` - 局面
/// * `us` - 攻撃側の色
pub fn attacks_slider(pos: &Position, us: Color) -> Bitboard {
    let occ = pos.occupied();
    let mut sum = Bitboard::EMPTY;

    for from in pos.pieces(us, PieceType::Lance).iter() {
        sum |= lance_effect(us, from, occ);
    }
    for from in pos.pieces(us, PieceType::Bishop).iter() {
        sum |= bishop_effect(from, occ);
    }
    for from in pos.pieces(us, PieceType::Horse).iter() {
        sum |= horse_effect(from, occ);
    }
    for from in pos.pieces(us, PieceType::Rook).iter() {
        sum |= rook_effect(from, occ);
    }
    for from in pos.pieces(us, PieceType::Dragon).iter() {
        sum |= dragon_effect(from, occ);
    }

    sum
}

/// Sliderの利きを列挙する（avoid升の駒を除外）
///
/// # Arguments
/// * `pos` - 局面
/// * `us` - 攻撃側の色
/// * `avoid_from` - 除外する升
/// * `occ` - 占有bitboard
pub fn attacks_slider_avoiding(
    pos: &Position,
    us: Color,
    avoid_from: Square,
    occ: Bitboard,
) -> Bitboard {
    let avoid = !Bitboard::from_square(avoid_from);
    let mut sum = Bitboard::EMPTY;

    for from in (pos.pieces(us, PieceType::Lance) & avoid).iter() {
        sum |= lance_effect(us, from, occ);
    }
    for from in
        ((pos.pieces(us, PieceType::Bishop) | pos.pieces(us, PieceType::Horse)) & avoid).iter()
    {
        let is_horse = pos.pieces(us, PieceType::Horse).contains(from);
        sum |= if is_horse {
            horse_effect(from, occ)
        } else {
            bishop_effect(from, occ)
        };
    }
    for from in
        ((pos.pieces(us, PieceType::Rook) | pos.pieces(us, PieceType::Dragon)) & avoid).iter()
    {
        let is_dragon = pos.pieces(us, PieceType::Dragon).contains(from);
        sum |= if is_dragon {
            dragon_effect(from, occ)
        } else {
            rook_effect(from, occ)
        };
    }

    sum
}

/// 玉周辺の利き（NonSliderのみ）
///
/// # Arguments
/// * `pos` - 局面
/// * `our_king` - 自玉の色
pub fn attacks_around_king_non_slider(pos: &Position, our_king: Color) -> Bitboard {
    let them = !our_king;
    let mut sum = Bitboard::EMPTY;
    for from in pos.pieces(them, PieceType::Pawn).iter() {
        sum |= pawn_effect(them, from);
    }
    for from in pos.pieces(them, PieceType::Knight).iter() {
        sum |= knight_effect(them, from);
    }
    for from in pos.pieces(them, PieceType::Silver).iter() {
        sum |= silver_effect(them, from);
    }
    let gold_like = pos.pieces(them, PieceType::Gold)
        | pos.pieces(them, PieceType::ProPawn)
        | pos.pieces(them, PieceType::ProLance)
        | pos.pieces(them, PieceType::ProKnight)
        | pos.pieces(them, PieceType::ProSilver);
    for from in gold_like.iter() {
        sum |= gold_effect(them, from);
    }
    for from in pos.pieces(them, PieceType::King).iter() {
        sum |= king_effect(from);
    }
    // 馬・龍の近接部分もNonSlider相当として扱う
    for from in pos.pieces(them, PieceType::Horse).iter() {
        sum |= king_effect(from);
    }
    for from in pos.pieces(them, PieceType::Dragon).iter() {
        sum |= king_effect(from);
    }

    // 盤面外は含まれないが、玉自身を除く
    sum &= !Bitboard::from_square(pos.king_square(our_king));
    // occは現在の占有。NonSliderなので遮断は考慮不要。
    sum
}

/// 玉周辺の利き（Sliderのみ）
///
/// # Arguments
/// * `pos` - 局面
/// * `our_king` - 自玉の色
pub fn attacks_around_king_slider(pos: &Position, our_king: Color) -> Bitboard {
    let them = !our_king;
    let occ = pos.occupied();
    let mut sum = Bitboard::EMPTY;

    for from in pos.pieces(them, PieceType::Lance).iter() {
        sum |= lance_effect(them, from, occ);
    }
    for from in pos.pieces(them, PieceType::Bishop).iter() {
        sum |= bishop_effect(from, occ);
    }
    for from in pos.pieces(them, PieceType::Horse).iter() {
        sum |= bishop_effect(from, occ);
    }
    for from in pos.pieces(them, PieceType::Rook).iter() {
        sum |= rook_effect(from, occ);
    }
    for from in pos.pieces(them, PieceType::Dragon).iter() {
        sum |= rook_effect(from, occ);
    }

    sum
}

/// 玉周辺の利き（fromの駒を除外）
///
/// # Arguments
/// * `pos` - 局面
/// * `us` - 自玉の色
/// * `from` - 除外する升
/// * `occ` - 占有bitboard
pub fn attacks_around_king_in_avoiding(
    pos: &Position,
    us: Color,
    from: Square,
    occ: Bitboard,
) -> Bitboard {
    let avoid = !Bitboard::from_square(from);
    let them = !us;
    let mut sum = Bitboard::EMPTY;

    // NonSlider（avoid適用）
    for sq in (pos.pieces(them, PieceType::Pawn) & avoid).iter() {
        sum |= pawn_effect(them, sq);
    }
    for sq in (pos.pieces(them, PieceType::Knight) & avoid).iter() {
        sum |= knight_effect(them, sq);
    }
    for sq in (pos.pieces(them, PieceType::Silver) & avoid).iter() {
        sum |= silver_effect(them, sq);
    }
    let gold_like = (pos.pieces(them, PieceType::Gold)
        | pos.pieces(them, PieceType::ProPawn)
        | pos.pieces(them, PieceType::ProLance)
        | pos.pieces(them, PieceType::ProKnight)
        | pos.pieces(them, PieceType::ProSilver))
        & avoid;
    for sq in gold_like.iter() {
        sum |= gold_effect(them, sq);
    }
    for sq in (pos.pieces(them, PieceType::King) & avoid).iter() {
        sum |= king_effect(sq);
    }

    // Slider（avoid適用）
    for sq in (pos.pieces(them, PieceType::Lance) & avoid).iter() {
        sum |= lance_effect(them, sq, occ);
    }
    for sq in (pos.pieces(them, PieceType::Bishop) & avoid).iter() {
        sum |= bishop_effect(sq, occ);
    }
    for sq in (pos.pieces(them, PieceType::Horse) & avoid).iter() {
        sum |= horse_effect(sq, occ);
    }
    for sq in (pos.pieces(them, PieceType::Rook) & avoid).iter() {
        sum |= rook_effect(sq, occ);
    }
    for sq in (pos.pieces(them, PieceType::Dragon) & avoid).iter() {
        sum |= dragon_effect(sq, occ);
    }

    sum
}

/// 玉がtoとbb_avoid以外の升に逃げられるか
///
/// # Arguments
/// * `pos` - 局面
/// * `us` - 玉の色
/// * `to` - 駒を打つ/移動する升
/// * `bb_avoid` - 逃げられない升
/// * `slide` - 占有bitboard
///
/// # Returns
/// 逃げられる場合は`true`
pub fn can_king_escape(
    pos: &Position,
    us: Color,
    to: Square,
    bb_avoid: Bitboard,
    slide: Bitboard,
) -> bool {
    let king_sq = pos.king_square(us);
    let mut escape = king_effect(king_sq);
    escape &= !bb_avoid;
    escape &= !Bitboard::from_square(to);
    escape &= !pos.pieces_c(us);

    for dest in escape.iter() {
        let attacked = pos.attackers_to_occ(dest, slide) & pos.pieces_c(!us);
        if attacked.is_empty() {
            return true;
        }
    }
    false
}

/// 玉がtoとbb_avoid以外の升に逃げられるか（fromから駒を除去）
///
/// # Arguments
/// * `pos` - 局面
/// * `us` - 玉の色
/// * `from` - 駒を除去する升
/// * `to` - 駒を移動する升
/// * `bb_avoid` - 逃げられない升
/// * `slide` - 占有bitboard
///
/// # Returns
/// 逃げられる場合は`true`
pub fn can_king_escape_with_from(
    pos: &Position,
    us: Color,
    from: Square,
    to: Square,
    bb_avoid: Bitboard,
    slide: Bitboard,
) -> bool {
    let king_sq = pos.king_square(us);
    let mut escape = king_effect(king_sq);
    escape &= !bb_avoid;
    escape &= !Bitboard::from_square(to);
    escape &= !pos.pieces_c(us);

    for dest in escape.iter() {
        let mut occ = slide;
        // exclude moving piece from attack evaluation
        occ &= !Bitboard::from_square(from);
        let attacked = pos.attackers_to_occ(dest, occ) & pos.pieces_c(!us);
        if attacked.is_empty() {
            return true;
        }
    }
    false
}

/// 玉以外の駒でtoの駒が取れるか
///
/// # Arguments
/// * `pos` - 局面
/// * `us` - 玉の色
/// * `to` - 取る対象の升
/// * `pinned` - Pin駒のbitboard
/// * `slide` - 占有bitboard
///
/// # Returns
/// 取れる場合は`true`
pub fn can_piece_capture(
    pos: &Position,
    us: Color,
    to: Square,
    pinned: Bitboard,
    slide: Bitboard,
) -> bool {
    let king_sq = pos.king_square(us);
    let mut attackers = pos.attackers_to_occ(to, slide) & pos.pieces_c(us);
    attackers &= !Bitboard::from_square(king_sq);

    for from in attackers.iter() {
        if pinned.contains(from) && !aligned(from, to, king_sq) {
            continue;
        }
        return true;
    }
    false
}

/// 玉以外の駒でtoの駒が取れるか（avoid升の駒を除外）
///
/// # Arguments
/// * `pos` - 局面
/// * `us` - 玉の色
/// * `to` - 取る対象の升
/// * `avoid` - 除外する升
/// * `pinned` - Pin駒のbitboard
/// * `slide` - 占有bitboard
///
/// # Returns
/// 取れる場合は`true`
pub fn can_piece_capture_avoiding(
    pos: &Position,
    us: Color,
    to: Square,
    avoid: Square,
    pinned: Bitboard,
    slide: Bitboard,
) -> bool {
    let king_sq = pos.king_square(us);
    let avoid_bb = Bitboard::from_square(avoid);
    let mut attackers = pos.attackers_to_occ(to, slide) & pos.pieces_c(us);
    attackers &= !avoid_bb;
    attackers &= !Bitboard::from_square(king_sq);

    for from in attackers.iter() {
        if pinned.contains(from) && !aligned(from, to, king_sq) {
            continue;
        }
        return true;
    }
    false
}

/// 歩が打てるか（二歩チェック含む）
///
/// # Arguments
/// * `pos` - 局面
/// * `us` - 打つ側の色
/// * `sq` - 打つ升
///
/// # Returns
/// 打てる場合は`true`
pub fn can_pawn_drop(pos: &Position, us: Color, sq: Square) -> bool {
    if !pos.hand(us).has(PieceType::Pawn) {
        return false;
    }

    // 同筋に自分の歩があると二歩
    let file_bb = FILE_BB[sq.file().index()];
    if !(pos.pieces(us, PieceType::Pawn) & file_bb).is_empty() {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_helper_functions_compile() {
        // ヘルパー関数がコンパイルされることを確認
    }
}
