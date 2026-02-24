// 1手詰め探索用のヘルパー関数

use crate::bitboard::{
    Bitboard, FILE_BB, bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect,
    knight_effect, lance_effect, pawn_effect, rook_effect, silver_effect,
};
use crate::mate::tables::check_around_bb;
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
    // BISHOP_HORSE統合 — Slider利きのみ(bishopEffect)で十分
    for from in
        ((pos.pieces(us, PieceType::Bishop) | pos.pieces(us, PieceType::Horse)) & avoid).iter()
    {
        sum |= bishop_effect(from, occ);
    }
    // ROOK_DRAGON統合 — Slider利きのみ(rookEffect)で十分
    for from in
        ((pos.pieces(us, PieceType::Rook) | pos.pieces(us, PieceType::Dragon)) & avoid).iter()
    {
        sum |= rook_effect(from, occ);
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
    let sq_king = pos.king_square(our_king);
    let mut sum = Bitboard::EMPTY;
    for from in pos.pieces(them, PieceType::Pawn).iter() {
        sum |= pawn_effect(them, from);
    }
    let knights =
        pos.pieces(them, PieceType::Knight) & check_around_bb(them, PieceType::Knight, sq_king);
    for from in knights.iter() {
        sum |= knight_effect(them, from);
    }
    let silvers =
        pos.pieces(them, PieceType::Silver) & check_around_bb(them, PieceType::Silver, sq_king);
    for from in silvers.iter() {
        sum |= silver_effect(them, from);
    }
    let gold_like = (pos.pieces(them, PieceType::Gold)
        | pos.pieces(them, PieceType::ProPawn)
        | pos.pieces(them, PieceType::ProLance)
        | pos.pieces(them, PieceType::ProKnight)
        | pos.pieces(them, PieceType::ProSilver))
        & check_around_bb(them, PieceType::Gold, sq_king);
    for from in gold_like.iter() {
        sum |= gold_effect(them, from);
    }
    let hdk = (pos.pieces(them, PieceType::King)
        | pos.pieces(them, PieceType::Horse)
        | pos.pieces(them, PieceType::Dragon))
        & check_around_bb(them, PieceType::King, sq_king);
    for from in hdk.iter() {
        sum |= king_effect(from);
    }

    // 自玉位置の除外は行わない（呼び出し元で自然に除外される）
    sum
}

/// 玉周辺の利き（Sliderのみ）
///
/// # Arguments
/// * `pos` - 局面
/// * `our_king` - 自玉の色
pub fn attacks_around_king_slider(pos: &Position, our_king: Color) -> Bitboard {
    let them = !our_king;
    let sq_king = pos.king_square(our_king);
    let occ = pos.occupied();
    let mut sum = Bitboard::EMPTY;

    let lances =
        pos.pieces(them, PieceType::Lance) & check_around_bb(them, PieceType::Lance, sq_king);
    for from in lances.iter() {
        sum |= lance_effect(them, from, occ);
    }
    let bishops = (pos.pieces(them, PieceType::Bishop) | pos.pieces(them, PieceType::Horse))
        & check_around_bb(them, PieceType::Bishop, sq_king);
    for from in bishops.iter() {
        sum |= bishop_effect(from, occ);
    }
    let rooks = (pos.pieces(them, PieceType::Rook) | pos.pieces(them, PieceType::Dragon))
        & check_around_bb(them, PieceType::Rook, sq_king);
    for from in rooks.iter() {
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
    let them = !us;
    attacks_around_king_non_slider_in_avoiding(pos, them, us, from)
        | attacks_slider_avoiding(pos, them, from, occ)
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
    let slide = slide | Bitboard::from_square(to);
    // toは王手駒のマスだが、王がそこに移動して駒を取ることで逃げられるため除外しない
    let escape = king_effect(king_sq) & !(bb_avoid | pos.pieces_c(us));

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
    let slide = (slide | Bitboard::from_square(to)) & !Bitboard::from_square(king_sq);
    // toには攻め駒が移動してきているので逃げ先から除外する。
    // (toの駒はposと異なる成り駒の可能性があり、attackers_toが正しい利きを返さないため)
    let escape = king_effect(king_sq) & !(bb_avoid | Bitboard::from_square(to) | pos.pieces_c(us));

    for dest in escape.iter() {
        let attacked = pos.attackers_to_occ(dest, slide) & pos.pieces_c(!us);
        let attacked_wo_from = attacked & !Bitboard::from_square(from);
        if attacked_wo_from.is_empty() {
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

fn attacks_around_king_non_slider_in_avoiding(
    pos: &Position,
    them: Color,
    our_king: Color,
    avoid: Square,
) -> Bitboard {
    let sq_king = pos.king_square(our_king);
    let mut sum = Bitboard::EMPTY;
    let avoid_bb = !Bitboard::from_square(avoid);

    // 歩はavoid_bbを適用しない（一括シフトで計算、avoid駒が歩でも除外しない）
    for from in pos.pieces(them, PieceType::Pawn).iter() {
        sum |= pawn_effect(them, from);
    }
    let knights = (pos.pieces(them, PieceType::Knight)
        & check_around_bb(them, PieceType::Knight, sq_king))
        & avoid_bb;
    for from in knights.iter() {
        sum |= knight_effect(them, from);
    }
    let silvers = (pos.pieces(them, PieceType::Silver)
        & check_around_bb(them, PieceType::Silver, sq_king))
        & avoid_bb;
    for from in silvers.iter() {
        sum |= silver_effect(them, from);
    }
    let gold_like = (pos.pieces(them, PieceType::Gold)
        | pos.pieces(them, PieceType::ProPawn)
        | pos.pieces(them, PieceType::ProLance)
        | pos.pieces(them, PieceType::ProKnight)
        | pos.pieces(them, PieceType::ProSilver))
        & check_around_bb(them, PieceType::Gold, sq_king)
        & avoid_bb;
    for from in gold_like.iter() {
        sum |= gold_effect(them, from);
    }
    let hdk = (pos.pieces(them, PieceType::King)
        | pos.pieces(them, PieceType::Horse)
        | pos.pieces(them, PieceType::Dragon))
        & check_around_bb(them, PieceType::King, sq_king)
        & avoid_bb;
    for from in hdk.iter() {
        sum |= king_effect(from);
    }

    sum
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_helper_functions_compile() {
        // ヘルパー関数がコンパイルされることを確認
    }
}
