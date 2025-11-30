//! 遠方駒（香、角、飛）の利き計算

use std::array;
use std::sync::OnceLock;

use crate::types::{Color, File, Rank, Square};

use super::Bitboard;

struct SliderTable {
    rook_masks: [Vec<Square>; Square::NUM],
    rook_attacks: [Vec<Bitboard>; Square::NUM],
    bishop_masks: [Vec<Square>; Square::NUM],
    bishop_attacks: [Vec<Bitboard>; Square::NUM],
    lance_forward: [[Bitboard; Square::NUM]; Color::NUM],
}

static SLIDER_ATTACKS: OnceLock<SliderTable> = OnceLock::new();

fn slider_attacks() -> &'static SliderTable {
    SLIDER_ATTACKS.get_or_init(SliderTable::new)
}

impl SliderTable {
    fn new() -> Self {
        let mut rook_masks: [Vec<Square>; Square::NUM] = array::from_fn(|_| Vec::new());
        let mut rook_attacks: [Vec<Bitboard>; Square::NUM] = array::from_fn(|_| Vec::new());
        let mut bishop_masks: [Vec<Square>; Square::NUM] = array::from_fn(|_| Vec::new());
        let mut bishop_attacks: [Vec<Bitboard>; Square::NUM] = array::from_fn(|_| Vec::new());
        let mut lance_forward = [[Bitboard::EMPTY; Square::NUM]; Color::NUM];

        for sq in Square::all() {
            let idx = sq.index();

            let rook_rays = build_rays(sq, &[(0, -1), (0, 1), (1, 0), (-1, 0)]);
            let rook_mask = flatten_rays(&rook_rays);
            rook_masks[idx] = rook_mask.clone();
            rook_attacks[idx] = build_attack_table(&rook_rays, &rook_mask);

            let bishop_rays = build_rays(sq, &[(1, -1), (-1, -1), (1, 1), (-1, 1)]);
            let bishop_mask = flatten_rays(&bishop_rays);
            bishop_masks[idx] = bishop_mask.clone();
            bishop_attacks[idx] = build_attack_table(&bishop_rays, &bishop_mask);
        }

        for color in [Color::Black, Color::White] {
            for sq in Square::all() {
                lance_forward[color.index()][sq.index()] = forward_ray(color, sq);
            }
        }

        SliderTable {
            rook_masks,
            rook_attacks,
            bishop_masks,
            bishop_attacks,
            lance_forward,
        }
    }
}

fn build_rays(sq: Square, dirs: &[(i32, i32)]) -> Vec<Vec<Square>> {
    dirs.iter().map(|&(df, dr)| ray(sq, df, dr)).collect()
}

fn ray(sq: Square, df: i32, dr: i32) -> Vec<Square> {
    let mut squares = Vec::new();
    let mut file = sq.file() as i32 + df;
    let mut rank = sq.rank() as i32 + dr;
    while in_bounds(file, rank) {
        squares.push(Square::new(
            File::from_u8(file as u8).unwrap(),
            Rank::from_u8(rank as u8).unwrap(),
        ));
        file += df;
        rank += dr;
    }
    squares
}

fn flatten_rays(rays: &[Vec<Square>]) -> Vec<Square> {
    rays.iter().flat_map(|v| v.iter().copied()).collect()
}

fn build_attack_table(rays: &[Vec<Square>], mask: &[Square]) -> Vec<Bitboard> {
    debug_assert!(mask.len() < usize::BITS as usize);
    let table_len = 1usize << mask.len();
    let mut table = Vec::with_capacity(table_len);
    for idx in 0..table_len {
        let occupied = occupancy_from_index(idx, mask);
        let attacks = attacks_from_rays(rays, occupied);
        table.push(attacks);
    }
    table
}

fn occupancy_from_index(index: usize, mask: &[Square]) -> Bitboard {
    let mut bb = Bitboard::EMPTY;
    for (i, sq) in mask.iter().enumerate() {
        if (index >> i) & 1 == 1 {
            bb.set(*sq);
        }
    }
    bb
}

fn occupancy_to_index(occupied: Bitboard, mask: &[Square]) -> usize {
    let mut idx = 0usize;
    for (i, sq) in mask.iter().enumerate() {
        if occupied.contains(*sq) {
            idx |= 1usize << i;
        }
    }
    idx
}

fn attacks_from_rays(rays: &[Vec<Square>], occupied: Bitboard) -> Bitboard {
    let mut result = Bitboard::EMPTY;
    for ray in rays {
        for &target in ray {
            result.set(target);
            if occupied.contains(target) {
                break;
            }
        }
    }
    result
}

fn forward_ray(color: Color, sq: Square) -> Bitboard {
    let dir = if color == Color::Black {
        (0, -1)
    } else {
        (0, 1)
    };
    let mut result = Bitboard::EMPTY;
    let mut file = sq.file() as i32 + dir.0;
    let mut rank = sq.rank() as i32 + dir.1;
    while in_bounds(file, rank) {
        let target =
            Square::new(File::from_u8(file as u8).unwrap(), Rank::from_u8(rank as u8).unwrap());
        result.set(target);
        file += dir.0;
        rank += dir.1;
    }
    result
}

#[inline]
fn in_bounds(file: i32, rank: i32) -> bool {
    (0..=8).contains(&file) && (0..=8).contains(&rank)
}

/// 香の利きを計算
///
/// # Arguments
/// * `color` - 先手/後手
/// * `sq` - 駒の位置
/// * `occupied` - 盤上の駒があるマスのBitboard
#[inline]
pub fn lance_effect(color: Color, sq: Square, occupied: Bitboard) -> Bitboard {
    let table = slider_attacks();
    let forward = table.lance_forward[color.index()][sq.index()];
    rook_effect(sq, occupied) & forward
}

/// 角の利きを計算
#[inline]
pub fn bishop_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    let table = slider_attacks();
    let mask = &table.bishop_masks[sq.index()];
    let idx = occupancy_to_index(occupied, mask);
    table.bishop_attacks[sq.index()][idx]
}

/// 飛車の利きを計算
#[inline]
pub fn rook_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    let table = slider_attacks();
    let mask = &table.rook_masks[sq.index()];
    let idx = occupancy_to_index(occupied, mask);
    table.rook_attacks[sq.index()][idx]
}

/// 馬の利きを計算（角の利き + 王の利き）
#[inline]
pub fn horse_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    bishop_effect(sq, occupied) | super::king_effect(sq)
}

/// 龍の利きを計算（飛車の利き + 王の利き）
#[inline]
pub fn dragon_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    rook_effect(sq, occupied) | super::king_effect(sq)
}

/// 2マス間のBitboard（両端を含まない）
pub fn between_bb(sq1: Square, sq2: Square) -> Bitboard {
    let idx1 = sq1.index() as i32;
    let idx2 = sq2.index() as i32;

    if idx1 == idx2 {
        return Bitboard::EMPTY;
    }

    let file1 = idx1 / 9;
    let rank1 = idx1 % 9;
    let file2 = idx2 / 9;
    let rank2 = idx2 % 9;

    let file_diff = file2 - file1;
    let rank_diff = rank2 - rank1;

    // 同一直線上にない場合は空
    if file_diff != 0 && rank_diff != 0 && file_diff.abs() != rank_diff.abs() {
        return Bitboard::EMPTY;
    }

    let file_step = file_diff.signum();
    let rank_step = rank_diff.signum();

    let mut result = Bitboard::EMPTY;
    let mut f = file1 + file_step;
    let mut r = rank1 + rank_step;

    while f != file2 || r != rank2 {
        let idx = f * 9 + r;
        if (0..81).contains(&idx) {
            result.set(unsafe { Square::from_u8_unchecked(idx as u8) });
        }
        f += file_step;
        r += rank_step;
    }

    result
}

/// 2マスを通る直線上のBitboard
pub fn line_bb(sq1: Square, sq2: Square) -> Bitboard {
    let idx1 = sq1.index() as i32;
    let idx2 = sq2.index() as i32;

    if idx1 == idx2 {
        return Bitboard::EMPTY;
    }

    let file1 = idx1 / 9;
    let rank1 = idx1 % 9;
    let file2 = idx2 / 9;
    let rank2 = idx2 % 9;

    let file_diff = file2 - file1;
    let rank_diff = rank2 - rank1;

    // 同一直線上にない場合は空
    if file_diff != 0 && rank_diff != 0 && file_diff.abs() != rank_diff.abs() {
        return Bitboard::EMPTY;
    }

    // 同じ筋
    if file_diff == 0 {
        return super::FILE_BB[file1 as usize];
    }

    // 同じ段
    if rank_diff == 0 {
        return super::RANK_BB[rank1 as usize];
    }

    // 斜め
    let file_step = file_diff.signum();
    let rank_step = rank_diff.signum();

    let mut result = Bitboard::EMPTY;

    // sq1から逆方向に伸ばす
    let mut f = file1;
    let mut r = rank1;
    while (0..=8).contains(&f) && (0..=8).contains(&r) {
        let idx = f * 9 + r;
        result.set(unsafe { Square::from_u8_unchecked(idx as u8) });
        f -= file_step;
        r -= rank_step;
    }

    // sq1から順方向に伸ばす
    f = file1 + file_step;
    r = rank1 + rank_step;
    while (0..=8).contains(&f) && (0..=8).contains(&r) {
        let idx = f * 9 + r;
        result.set(unsafe { Square::from_u8_unchecked(idx as u8) });
        f += file_step;
        r += rank_step;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Color, File, Rank};

    fn slider_naive(sq: Square, occupied: Bitboard, dirs: &[(i32, i32)]) -> Bitboard {
        let mut result = Bitboard::EMPTY;
        let file = sq.file() as i32;
        let rank = sq.rank() as i32;

        for (df, dr) in dirs {
            let mut f = file + df;
            let mut r = rank + dr;
            while (0..=8).contains(&f) && (0..=8).contains(&r) {
                let target =
                    Square::new(File::from_u8(f as u8).unwrap(), Rank::from_u8(r as u8).unwrap());
                result.set(target);
                if occupied.contains(target) {
                    break;
                }
                f += df;
                r += dr;
            }
        }

        result
    }

    fn rook_naive(sq: Square, occupied: Bitboard) -> Bitboard {
        slider_naive(sq, occupied, &[(0, -1), (0, 1), (1, 0), (-1, 0)])
    }

    fn bishop_naive(sq: Square, occupied: Bitboard) -> Bitboard {
        slider_naive(sq, occupied, &[(1, -1), (-1, -1), (1, 1), (-1, 1)])
    }

    fn lance_naive(color: Color, sq: Square, occupied: Bitboard) -> Bitboard {
        let dir = if color == Color::Black {
            (0, -1)
        } else {
            (0, 1)
        };
        slider_naive(sq, occupied, &[dir])
    }

    fn rand64(state: &mut u64) -> u64 {
        *state ^= *state << 7;
        *state ^= *state >> 9;
        *state ^= *state << 8;
        *state
    }

    fn random_bitboard(state: &mut u64) -> Bitboard {
        let mut bb = Bitboard::EMPTY;
        for sq in Square::all() {
            if rand64(state) & 1 == 1 {
                bb.set(sq);
            }
        }
        bb
    }

    #[test]
    fn test_lance_effect_black() {
        // 先手5五の香 -> 5四、5三、5二、5一に利き（遮蔽なし）
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = lance_effect(Color::Black, sq55, Bitboard::EMPTY);
        assert_eq!(bb.count(), 4);
        assert!(bb.contains(Square::new(File::File5, Rank::Rank4)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank3)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank2)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank1)));
    }

    #[test]
    fn test_lance_effect_black_blocked() {
        // 先手5五の香、5三に駒がある -> 5四、5三に利き
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq53 = Square::new(File::File5, Rank::Rank3);
        let occupied = Bitboard::from_square(sq53);
        let bb = lance_effect(Color::Black, sq55, occupied);
        assert_eq!(bb.count(), 2);
        assert!(bb.contains(Square::new(File::File5, Rank::Rank4)));
        assert!(bb.contains(sq53)); // 駒のあるマスにも利く
    }

    #[test]
    fn test_lance_effect_white() {
        // 後手5五の香 -> 5六、5七、5八、5九に利き
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = lance_effect(Color::White, sq55, Bitboard::EMPTY);
        assert_eq!(bb.count(), 4);
        assert!(bb.contains(Square::new(File::File5, Rank::Rank6)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank7)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank8)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank9)));
    }

    #[test]
    fn test_bishop_effect() {
        // 5五の角 -> 4方向の斜めに利き
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = bishop_effect(sq55, Bitboard::EMPTY);
        // 斜め4方向、各4マス = 16マス
        assert_eq!(bb.count(), 16);

        // 左上方向
        assert!(bb.contains(Square::new(File::File6, Rank::Rank4)));
        assert!(bb.contains(Square::new(File::File7, Rank::Rank3)));
        // 右上方向
        assert!(bb.contains(Square::new(File::File4, Rank::Rank4)));
        assert!(bb.contains(Square::new(File::File3, Rank::Rank3)));
        // 左下方向
        assert!(bb.contains(Square::new(File::File6, Rank::Rank6)));
        // 右下方向
        assert!(bb.contains(Square::new(File::File4, Rank::Rank6)));
    }

    #[test]
    fn test_bishop_effect_blocked() {
        // 5五の角、6四に駒がある
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq64 = Square::new(File::File6, Rank::Rank4);
        let occupied = Bitboard::from_square(sq64);
        let bb = bishop_effect(sq55, occupied);

        // 左上方向は6四で止まる
        assert!(bb.contains(sq64));
        assert!(!bb.contains(Square::new(File::File7, Rank::Rank3)));
    }

    #[test]
    fn test_bishop_effect_corner() {
        let sq11 = Square::new(File::File1, Rank::Rank1);
        let bb = bishop_effect(sq11, Bitboard::EMPTY);
        assert_eq!(bb.count(), 8);
        assert!(bb.contains(Square::new(File::File2, Rank::Rank2)));
        assert!(bb.contains(Square::new(File::File9, Rank::Rank9)));
        assert!(!bb.contains(sq11));
    }

    #[test]
    fn test_rook_effect() {
        // 5五の飛車 -> 縦横に利き
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = rook_effect(sq55, Bitboard::EMPTY);
        // 縦8マス + 横8マス = 16マス
        assert_eq!(bb.count(), 16);

        // 上方向
        assert!(bb.contains(Square::new(File::File5, Rank::Rank4)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank1)));
        // 下方向
        assert!(bb.contains(Square::new(File::File5, Rank::Rank6)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank9)));
        // 左方向
        assert!(bb.contains(Square::new(File::File6, Rank::Rank5)));
        // 右方向
        assert!(bb.contains(Square::new(File::File4, Rank::Rank5)));
    }

    #[test]
    fn test_rook_effect_corner() {
        let sq11 = Square::new(File::File1, Rank::Rank1);
        let bb = rook_effect(sq11, Bitboard::EMPTY);
        assert_eq!(bb.count(), 16);
        assert!(bb.contains(Square::new(File::File1, Rank::Rank9)));
        assert!(bb.contains(Square::new(File::File9, Rank::Rank1)));
        assert!(!bb.contains(sq11));
    }

    #[test]
    fn test_horse_effect() {
        // 馬 = 角 + 王
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = horse_effect(sq55, Bitboard::EMPTY);
        // 斜め16マス + 隣接8マス（ただし4マスは重複）= 20マス
        assert_eq!(bb.count(), 20);
    }

    #[test]
    fn test_dragon_effect() {
        // 龍 = 飛車 + 王
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = dragon_effect(sq55, Bitboard::EMPTY);
        // 縦横16マス + 隣接8マス（ただし4マスは重複）= 20マス
        assert_eq!(bb.count(), 20);
    }

    #[test]
    fn test_between_bb() {
        // 5五と5一の間 -> 5四、5三、5二
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq51 = Square::new(File::File5, Rank::Rank1);
        let bb = between_bb(sq55, sq51);
        assert_eq!(bb.count(), 3);
        assert!(bb.contains(Square::new(File::File5, Rank::Rank4)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank3)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank2)));

        // 隣接マスの間は空
        let sq54 = Square::new(File::File5, Rank::Rank4);
        let bb = between_bb(sq55, sq54);
        assert!(bb.is_empty());

        // 同一マスの場合は空
        let bb = between_bb(sq55, sq55);
        assert!(bb.is_empty());

        // 直線上にない場合は空
        let sq64 = Square::new(File::File6, Rank::Rank4);
        let bb = between_bb(sq55, sq64);
        assert!(bb.is_empty());
    }

    #[test]
    fn test_line_bb() {
        // 5五と5一を通る直線 -> 5筋全体
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq51 = Square::new(File::File5, Rank::Rank1);
        let bb = line_bb(sq55, sq51);
        assert_eq!(bb.count(), 9);

        // 斜め
        let sq64 = Square::new(File::File6, Rank::Rank4);
        let bb = line_bb(sq55, sq64);
        // 1九から9一への対角線
        assert!(bb.contains(sq55));
        assert!(bb.contains(sq64));
    }

    #[test]
    fn test_rook_effect_random_matches_naive() {
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        for _ in 0..32 {
            let occ = random_bitboard(&mut seed);
            for sq in Square::all() {
                let expected = rook_naive(sq, occ);
                assert_eq!(rook_effect(sq, occ), expected, "sq={:?}", sq);
            }
        }
    }

    #[test]
    fn test_bishop_effect_random_matches_naive() {
        let mut seed = 0x0F1E_2D3C_4B5A_6978u64;
        for _ in 0..32 {
            let occ = random_bitboard(&mut seed);
            for sq in Square::all() {
                let expected = bishop_naive(sq, occ);
                assert_eq!(bishop_effect(sq, occ), expected, "sq={:?}", sq);
            }
        }
    }

    #[test]
    fn test_lance_effect_random_matches_naive() {
        let mut seed = 0x55AA_A55Au64;
        for _ in 0..32 {
            let occ = random_bitboard(&mut seed);
            for sq in Square::all() {
                let expected_b = lance_naive(Color::Black, sq, occ);
                let expected_w = lance_naive(Color::White, sq, occ);
                assert_eq!(lance_effect(Color::Black, sq, occ), expected_b, "sq={:?}", sq);
                assert_eq!(lance_effect(Color::White, sq, occ), expected_w, "sq={:?}", sq);
            }
        }
    }
}
