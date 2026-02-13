//! 遠方駒（香・角・飛）の利きをYaneuraOu互換のQugiyアルゴリズムで計算する

use std::sync::OnceLock;

use crate::types::{Color, Square};

use super::utils::msb64;
use super::{Bitboard, Bitboard256, FILE_BB, RANK_BB};

/// 8方向の単一レイ（やねうら王のEffect8::Directに対応）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direct {
    RU = 0,
    R = 1,
    RD = 2,
    U = 3,
    D = 4,
    LU = 5,
    L = 6,
    LD = 7,
}

impl Direct {
    #[inline]
    const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Direct::RU),
            1 => Some(Direct::R),
            2 => Some(Direct::RD),
            3 => Some(Direct::U),
            4 => Some(Direct::D),
            5 => Some(Direct::LU),
            6 => Some(Direct::L),
            7 => Some(Direct::LD),
            _ => None,
        }
    }
}

struct SliderTable {
    lance_step_effect: [[Bitboard; Square::NUM]; Color::NUM],
    qugiy_rook_mask: [[Bitboard; 2]; Square::NUM],
    qugiy_bishop_mask: [[Bitboard256; 2]; Square::NUM],
    qugiy_step_effect: [[Bitboard; Square::NUM]; 6],
}

static SLIDER_ATTACKS: OnceLock<SliderTable> = OnceLock::new();

fn slider_attacks() -> &'static SliderTable {
    SLIDER_ATTACKS.get_or_init(SliderTable::new)
}

impl SliderTable {
    fn new() -> Self {
        let lance_step_effect = init_lance_step_effect();
        let qugiy_rook_mask = init_qugiy_rook_mask();
        let qugiy_bishop_mask = init_qugiy_bishop_mask();
        let qugiy_step_effect = init_qugiy_step_effect();

        SliderTable {
            lance_step_effect,
            qugiy_rook_mask,
            qugiy_bishop_mask,
            qugiy_step_effect,
        }
    }
}

fn in_bounds(file: i32, rank: i32) -> bool {
    (0..=8).contains(&file) && (0..=8).contains(&rank)
}

fn square_from_coords(file: i32, rank: i32) -> Square {
    debug_assert!(in_bounds(file, rank), "coordinates out of bounds");
    // SAFETY: 呼び出し元/上のassertで盤内を保証
    unsafe { Square::from_u8_unchecked((file * 9 + rank) as u8) }
}

fn init_lance_step_effect() -> [[Bitboard; Square::NUM]; Color::NUM] {
    let mut table = [[Bitboard::EMPTY; Square::NUM]; Color::NUM];

    for sq in Square::all() {
        let file = sq.file().index() as i32;
        let rank = sq.rank().index() as i32;

        // 先手: 前方(段-1方向)
        let mut bb_black = Bitboard::EMPTY;
        let mut r = rank - 1;
        while r >= 0 {
            bb_black.set(square_from_coords(file, r));
            r -= 1;
        }
        table[Color::Black.index()][sq.index()] = bb_black;

        // 後手: 前方(段+1方向)
        let mut bb_white = Bitboard::EMPTY;
        let mut r = rank + 1;
        while r < 9 {
            bb_white.set(square_from_coords(file, r));
            r += 1;
        }
        table[Color::White.index()][sq.index()] = bb_white;
    }

    table
}

fn init_qugiy_rook_mask() -> [[Bitboard; 2]; Square::NUM] {
    let mut mask = [[Bitboard::EMPTY; 2]; Square::NUM];

    for sq in Square::all() {
        let file = sq.file().index() as i32;
        let rank = sq.rank().index() as i32;

        // 左方向（file増加）
        let mut left = Bitboard::EMPTY;
        let mut f = file + 1;
        while f <= 8 {
            left.set(square_from_coords(f, rank));
            f += 1;
        }

        // 右方向（file減少）
        let mut right = Bitboard::EMPTY;
        let mut f = file - 1;
        while f >= 0 {
            right.set(square_from_coords(f, rank));
            f -= 1;
        }

        let right_rev = right.byte_reverse();
        let (hi, lo) = Bitboard::unpack(right_rev, left);

        mask[sq.index()][0] = lo;
        mask[sq.index()][1] = hi;
    }

    mask
}

fn init_qugiy_bishop_mask() -> [[Bitboard256; 2]; Square::NUM] {
    // 左上, 左下, 右上, 右下（rooksと同じくfile増加方向を「左」とみなす）
    const DIRS: [(i32, i32); 4] = [(1, -1), (1, 1), (-1, -1), (-1, 1)];
    let mut mask = [[Bitboard256::ZERO; 2]; Square::NUM];

    for sq in Square::all() {
        let file = sq.file().index() as i32;
        let rank = sq.rank().index() as i32;
        let mut step_effect = [Bitboard::EMPTY; 4];

        for (i, &(df, dr)) in DIRS.iter().enumerate() {
            let mut bb = Bitboard::EMPTY;
            let mut f = file + df;
            let mut r = rank + dr;
            while in_bounds(f, r) {
                bb.set(square_from_coords(f, r));
                f += df;
                r += dr;
            }
            step_effect[i] = bb;
        }

        // 右上・右下はbyte_reverseしておく
        step_effect[2] = step_effect[2].byte_reverse();
        step_effect[3] = step_effect[3].byte_reverse();

        let lo_pair = Bitboard::from_u64_pair(
            step_effect[0].extract64::<0>(),
            step_effect[2].extract64::<0>(),
        );
        let hi_pair = Bitboard::from_u64_pair(
            step_effect[1].extract64::<0>(),
            step_effect[3].extract64::<0>(),
        );
        mask[sq.index()][0] = Bitboard256::from_bitboards(lo_pair, hi_pair);

        let lo_pair = Bitboard::from_u64_pair(
            step_effect[0].extract64::<1>(),
            step_effect[2].extract64::<1>(),
        );
        let hi_pair = Bitboard::from_u64_pair(
            step_effect[1].extract64::<1>(),
            step_effect[3].extract64::<1>(),
        );
        mask[sq.index()][1] = Bitboard256::from_bitboards(lo_pair, hi_pair);
    }

    mask
}

fn init_qugiy_step_effect() -> [[Bitboard; Square::NUM]; 6] {
    // DIRECT_U/DIRECT_Dは持たない6方向。byte_reverse前提の方向はreverse=true。
    const STEP_DIRS: [(i32, i32); 6] = [
        (-1, -1), // 右上
        (-1, 0),  // 右
        (-1, 1),  // 右下
        (1, -1),  // 左上
        (1, 0),   // 左
        (1, 1),   // 左下
    ];

    let mut table = [[Bitboard::EMPTY; Square::NUM]; 6];

    for (dd, &(df, dr)) in STEP_DIRS.iter().enumerate() {
        let delta = df * 9 + dr;
        let reverse = delta < 0;

        for sq in Square::all() {
            let mut bb = Bitboard::EMPTY;
            let mut f = sq.file().index() as i32 + df;
            let mut r = sq.rank().index() as i32 + dr;
            while in_bounds(f, r) {
                bb.set(square_from_coords(f, r));
                f += df;
                r += dr;
            }

            table[dd][sq.index()] = if reverse { bb.byte_reverse() } else { bb };
        }
    }

    table
}

/// 盤上の駒を考慮しない香の利きレイ（方向別ルックアップテーブル）
/// YaneuraOu の lanceStepEffect 相当
#[inline]
pub fn lance_step_effect(color: Color, sq: Square) -> Bitboard {
    slider_attacks().lance_step_effect[color.index()][sq.index()]
}

/// 香の利きを計算（Qugiyアルゴリズム）
#[inline]
pub fn lance_effect(color: Color, sq: Square, occupied: Bitboard) -> Bitboard {
    let table = slider_attacks();
    let part = Bitboard::part(sq);
    let se = table.lance_step_effect[color.index()][sq.index()];

    match color {
        Color::White => {
            let mask = if part == 0 {
                se.extract64::<0>()
            } else {
                se.extract64::<1>()
            };
            let em = (if part == 0 {
                occupied.extract64::<0>()
            } else {
                occupied.extract64::<1>()
            }) & mask;
            let t = em.wrapping_sub(1);
            if part == 0 {
                Bitboard::from_u64_pair((em ^ t) & mask, 0)
            } else {
                Bitboard::from_u64_pair(0, (em ^ t) & mask)
            }
        }
        Color::Black => {
            let se_mask = if part == 0 {
                se.extract64::<0>()
            } else {
                se.extract64::<1>()
            };
            let mocc = se_mask
                & if part == 0 {
                    occupied.extract64::<0>()
                } else {
                    occupied.extract64::<1>()
                };
            let mocc = !0u64 << msb64(mocc | 1);
            if part == 0 {
                Bitboard::from_u64_pair(mocc & se_mask, 0)
            } else {
                Bitboard::from_u64_pair(0, mocc & se_mask)
            }
        }
    }
}

/// 飛車の縦利き（香の利きを合成）
#[inline]
pub fn rook_file_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    let table = slider_attacks();

    if Bitboard::part(sq) == 0 {
        let mask = table.lance_step_effect[Color::White.index()][sq.index()].extract64::<0>();
        let em = occupied.extract64::<0>() & mask;
        let t = em.wrapping_sub(1);

        let se = table.lance_step_effect[Color::Black.index()][sq.index()].extract64::<0>();
        let mocc = se & occupied.extract64::<0>();
        let mocc = !0u64 << msb64(mocc | 1);

        Bitboard::from_u64_pair(((em ^ t) & mask) | (mocc & se), 0)
    } else {
        let mask = table.lance_step_effect[Color::White.index()][sq.index()].extract64::<1>();
        let em = occupied.extract64::<1>() & mask;
        let t = em.wrapping_sub(1);

        let se = table.lance_step_effect[Color::Black.index()][sq.index()].extract64::<1>();
        let mocc = se & occupied.extract64::<1>();
        let mocc = !0u64 << msb64(mocc | 1);

        Bitboard::from_u64_pair(0, ((em ^ t) & mask) | (mocc & se))
    }
}

/// 飛車の横利き（Qugiyアルゴリズム）
#[inline]
pub fn rook_rank_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    let table = slider_attacks();
    let mask_lo = table.qugiy_rook_mask[sq.index()][0];
    let mask_hi = table.qugiy_rook_mask[sq.index()][1];

    let rocc = occupied.byte_reverse();
    let (hi, lo) = Bitboard::unpack(rocc, occupied);

    let hi = hi & mask_hi;
    let lo = lo & mask_lo;

    let (t1, t0) = Bitboard::decrement_pair(hi, lo);

    let t1 = (t1 ^ hi) & mask_hi;
    let t0 = (t0 ^ lo) & mask_lo;

    let (hi, lo) = Bitboard::unpack(t1, t0);

    hi.byte_reverse() | lo
}

/// 飛車の利き
#[inline]
pub fn rook_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    rook_rank_effect(sq, occupied) | rook_file_effect(sq, occupied)
}

/// 方向付きのレイ利き（やねうら王のrayEffectに相当）
#[inline]
pub fn ray_effect(dir: Direct, sq: Square, occupied: Bitboard) -> Bitboard {
    match dir {
        Direct::U => lance_effect(Color::Black, sq, occupied),
        Direct::D => lance_effect(Color::White, sq, occupied),
        _ => {
            let idx = match dir {
                Direct::RU => 0,
                Direct::R => 1,
                Direct::RD => 2,
                Direct::LU => 3,
                Direct::L => 4,
                Direct::LD => 5,
                Direct::U | Direct::D => unreachable!(),
            };
            let mask = slider_attacks().qugiy_step_effect[idx][sq.index()];
            let reverse = matches!(dir, Direct::RU | Direct::R | Direct::RD);

            let mut bb = occupied;
            if reverse {
                bb = bb.byte_reverse();
            }
            bb &= mask;
            let mut bb = (bb ^ bb.decrement()) & mask;
            if reverse {
                bb = bb.byte_reverse();
            }
            bb
        }
    }
}

/// 方向指定の部分利き（ray_effectのエイリアス）
#[inline]
pub fn direct_effect(sq: Square, dir: Direct, occupied: Bitboard) -> Bitboard {
    ray_effect(dir, sq, occupied)
}

/// 角の利き（Qugiyアルゴリズム）
#[inline]
pub fn bishop_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    let table = slider_attacks();
    let mask_lo = table.qugiy_bishop_mask[sq.index()][0];
    let mask_hi = table.qugiy_bishop_mask[sq.index()][1];

    let occ2 = Bitboard256::new(occupied);
    let rocc2 = Bitboard256::new(occupied.byte_reverse());

    let (hi, lo) = Bitboard256::unpack(rocc2, occ2);

    let hi = hi & mask_hi;
    let lo = lo & mask_lo;

    let (t1, t0) = Bitboard256::decrement_pair(hi, lo);

    let t1 = (t1 ^ hi) & mask_hi;
    let t0 = (t0 ^ lo) & mask_lo;

    let (hi, lo) = Bitboard256::unpack(t1, t0);

    (hi.byte_reverse() | lo).merge()
}

/// 馬の利き（角 + 王）
#[inline]
pub fn horse_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    bishop_effect(sq, occupied) | super::king_effect(sq)
}

/// 龍の利き（飛車 + 王）
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

    if file_diff != 0 && rank_diff != 0 && file_diff.abs() != rank_diff.abs() {
        return Bitboard::EMPTY;
    }

    if file_diff == 0 {
        return FILE_BB[file1 as usize];
    }

    if rank_diff == 0 {
        return RANK_BB[rank1 as usize];
    }

    let file_step = file_diff.signum();
    let rank_step = rank_diff.signum();

    let mut result = Bitboard::EMPTY;

    let mut f = file1;
    let mut r = rank1;
    while (0..=8).contains(&f) && (0..=8).contains(&r) {
        let idx = f * 9 + r;
        result.set(unsafe { Square::from_u8_unchecked(idx as u8) });
        f -= file_step;
        r -= rank_step;
    }

    let mut f = file1 + file_step;
    let mut r = rank1 + rank_step;
    while (0..=8).contains(&f) && (0..=8).contains(&r) {
        let idx = f * 9 + r;
        result.set(unsafe { Square::from_u8_unchecked(idx as u8) });
        f += file_step;
        r += rank_step;
    }

    result
}

/// sq1から見たsq2の方向（直線上/斜めのみ）を返す
#[inline]
pub fn direct_of(sq1: Square, sq2: Square) -> Option<Direct> {
    const fn signum(v: i32) -> i32 {
        if v < 0 {
            -1
        } else if v > 0 {
            1
        } else {
            0
        }
    }

    const fn build_direct_table() -> [[u8; Square::NUM]; Square::NUM] {
        let mut t = [[255u8; Square::NUM]; Square::NUM];
        let mut s1 = 0;
        while s1 < Square::NUM {
            let f1 = (s1 / 9) as i32;
            let r1 = (s1 % 9) as i32;
            let mut s2 = 0;
            while s2 < Square::NUM {
                let f2 = (s2 / 9) as i32;
                let r2 = (s2 % 9) as i32;
                let df = f2 - f1;
                let dr = r2 - r1;
                let dir = if df == 0 {
                    match signum(dr) {
                        -1 => Some(Direct::U),
                        1 => Some(Direct::D),
                        _ => None,
                    }
                } else if dr == 0 {
                    match signum(df) {
                        -1 => Some(Direct::R),
                        1 => Some(Direct::L),
                        _ => None,
                    }
                } else if (df * df) == (dr * dr) {
                    match (signum(df), signum(dr)) {
                        (-1, -1) => Some(Direct::RU),
                        (-1, 1) => Some(Direct::RD),
                        (1, -1) => Some(Direct::LU),
                        (1, 1) => Some(Direct::LD),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(d) = dir {
                    t[s1][s2] = d as u8;
                }
                s2 += 1;
            }
            s1 += 1;
        }
        t
    }

    static DIRECT_TABLE: [[u8; Square::NUM]; Square::NUM] = build_direct_table();

    Direct::from_u8(DIRECT_TABLE[sq1.index()][sq2.index()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Color, File, Rank};

    fn slider_naive(sq: Square, occupied: Bitboard, dirs: &[(i32, i32)]) -> Bitboard {
        let mut result = Bitboard::EMPTY;
        let file = sq.file().index() as i32;
        let rank = sq.rank().index() as i32;

        for (df, dr) in dirs {
            let mut f = file + df;
            let mut r = rank + dr;
            while (0..=8).contains(&f) && (0..=8).contains(&r) {
                let target = square_from_coords(f, r);
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
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq53 = Square::new(File::File5, Rank::Rank3);
        let occupied = Bitboard::from_square(sq53);
        let bb = lance_effect(Color::Black, sq55, occupied);
        assert_eq!(bb.count(), 2);
        assert!(bb.contains(Square::new(File::File5, Rank::Rank4)));
        assert!(bb.contains(sq53));
    }

    #[test]
    fn test_lance_effect_white() {
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
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = bishop_effect(sq55, Bitboard::EMPTY);
        assert_eq!(bb.count(), 16);

        assert!(bb.contains(Square::new(File::File6, Rank::Rank4)));
        assert!(bb.contains(Square::new(File::File7, Rank::Rank3)));
        assert!(bb.contains(Square::new(File::File4, Rank::Rank4)));
        assert!(bb.contains(Square::new(File::File3, Rank::Rank3)));
        assert!(bb.contains(Square::new(File::File6, Rank::Rank6)));
        assert!(bb.contains(Square::new(File::File4, Rank::Rank6)));
    }

    #[test]
    fn test_bishop_effect_blocked() {
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq64 = Square::new(File::File6, Rank::Rank4);
        let occupied = Bitboard::from_square(sq64);
        let bb = bishop_effect(sq55, occupied);

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
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = rook_effect(sq55, Bitboard::EMPTY);
        assert_eq!(bb.count(), 16);

        assert!(bb.contains(Square::new(File::File5, Rank::Rank4)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank1)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank6)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank9)));
        assert!(bb.contains(Square::new(File::File6, Rank::Rank5)));
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
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = horse_effect(sq55, Bitboard::EMPTY);
        assert_eq!(bb.count(), 20);
    }

    #[test]
    fn test_dragon_effect() {
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = dragon_effect(sq55, Bitboard::EMPTY);
        assert_eq!(bb.count(), 20);
    }

    #[test]
    fn test_between_bb() {
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq51 = Square::new(File::File5, Rank::Rank1);
        let bb = between_bb(sq55, sq51);
        assert_eq!(bb.count(), 3);
        assert!(bb.contains(Square::new(File::File5, Rank::Rank4)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank3)));
        assert!(bb.contains(Square::new(File::File5, Rank::Rank2)));

        let sq54 = Square::new(File::File5, Rank::Rank4);
        let bb = between_bb(sq55, sq54);
        assert!(bb.is_empty());

        let bb = between_bb(sq55, sq55);
        assert!(bb.is_empty());

        let sq64 = Square::new(File::File6, Rank::Rank4);
        let bb = between_bb(sq55, sq64);
        assert!(bb.is_empty());
    }

    #[test]
    fn test_line_bb() {
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq51 = Square::new(File::File5, Rank::Rank1);
        let bb = line_bb(sq55, sq51);
        assert_eq!(bb.count(), 9);

        let sq64 = Square::new(File::File6, Rank::Rank4);
        let bb = line_bb(sq55, sq64);
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

    #[test]
    fn test_direct_of_basic() {
        let c = Square::new(File::File5, Rank::Rank5);
        assert_eq!(direct_of(c, Square::new(File::File4, Rank::Rank4)), Some(Direct::RU));
        assert_eq!(direct_of(c, Square::new(File::File6, Rank::Rank5)), Some(Direct::L));
        assert_eq!(direct_of(c, Square::new(File::File5, Rank::Rank7)), Some(Direct::D));
        assert_eq!(direct_of(c, Square::new(File::File7, Rank::Rank4)), None);
    }

    #[test]
    fn test_ray_effect_matches_between_and_step() {
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let blocker = Square::new(File::File7, Rank::Rank7); // on LD ray
        let occ = Bitboard::from_square(blocker);

        let ray = ray_effect(Direct::LD, sq55, occ);
        let expected = between_bb(sq55, Square::new(File::File8, Rank::Rank8));
        assert_eq!(ray, expected);

        let ray_clear = ray_effect(Direct::LD, sq55, Bitboard::EMPTY);
        let mask = slider_attacks().qugiy_step_effect[5][sq55.index()];
        assert_eq!(ray_clear, mask);

        let up = ray_effect(Direct::U, sq55, Bitboard::EMPTY);
        let lance_up = lance_effect(Color::Black, sq55, Bitboard::EMPTY);
        assert_eq!(up, lance_up);
    }
}
