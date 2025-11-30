//! 遠方駒（香、角、飛）の利き計算

use crate::types::{Color, Square};

use super::Bitboard;

/// 方向を表す定数
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i8)]
pub enum Direction {
    /// 上（段が減る方向）
    Up = -1,
    /// 下（段が増える方向）
    Down = 1,
    /// 左（筋が増える方向）
    Left = 9,
    /// 右（筋が減る方向）
    Right = -9,
    /// 左上
    UpLeft = 8,
    /// 右上
    UpRight = -10,
    /// 左下
    DownLeft = 10,
    /// 右下
    DownRight = -8,
}

impl Direction {
    /// 方向のオフセット値を取得
    #[inline]
    pub const fn offset(self) -> i8 {
        self as i8
    }
}

/// 香の利きを計算
///
/// # Arguments
/// * `color` - 先手/後手
/// * `sq` - 駒の位置
/// * `occupied` - 盤上の駒があるマスのBitboard
#[inline]
pub fn lance_effect(color: Color, sq: Square, occupied: Bitboard) -> Bitboard {
    match color {
        Color::Black => sliding_effect_single(sq, occupied, Direction::Up),
        Color::White => sliding_effect_single(sq, occupied, Direction::Down),
    }
}

/// 角の利きを計算
#[inline]
pub fn bishop_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    sliding_effect_single(sq, occupied, Direction::UpLeft)
        | sliding_effect_single(sq, occupied, Direction::UpRight)
        | sliding_effect_single(sq, occupied, Direction::DownLeft)
        | sliding_effect_single(sq, occupied, Direction::DownRight)
}

/// 飛車の利きを計算
#[inline]
pub fn rook_effect(sq: Square, occupied: Bitboard) -> Bitboard {
    sliding_effect_single(sq, occupied, Direction::Up)
        | sliding_effect_single(sq, occupied, Direction::Down)
        | sliding_effect_single(sq, occupied, Direction::Left)
        | sliding_effect_single(sq, occupied, Direction::Right)
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

/// 単一方向の飛び利きを計算
fn sliding_effect_single(sq: Square, occupied: Bitboard, dir: Direction) -> Bitboard {
    let mut result = Bitboard::EMPTY;
    let offset = dir.offset() as i32;
    let mut current = sq.index() as i32 + offset;

    // 方向に応じた境界チェック
    while is_valid_slide(sq.index() as i32, current, dir) {
        let target_sq = unsafe { Square::from_u8_unchecked(current as u8) };
        result.set(target_sq);

        // 駒にぶつかったら停止
        if occupied.contains(target_sq) {
            break;
        }

        current += offset;
    }

    result
}

/// スライド移動が有効かどうかをチェック
#[inline]
fn is_valid_slide(from: i32, to: i32, dir: Direction) -> bool {
    if !(0..81).contains(&to) {
        return false;
    }

    let from_file = from / 9;
    let from_rank = from % 9;
    let to_file = to / 9;
    let to_rank = to % 9;

    match dir {
        Direction::Up => to_file == from_file && to_rank >= 0,
        Direction::Down => to_file == from_file && to_rank <= 8,
        Direction::Left => to_rank == from_rank && to_file <= 8,
        Direction::Right => to_rank == from_rank && to_file >= 0,
        Direction::UpLeft => {
            let file_diff = to_file - from_file;
            let rank_diff = to_rank - from_rank;
            file_diff == -rank_diff && file_diff > 0
        }
        Direction::UpRight => {
            let file_diff = to_file - from_file;
            let rank_diff = to_rank - from_rank;
            file_diff == rank_diff && file_diff < 0
        }
        Direction::DownLeft => {
            let file_diff = to_file - from_file;
            let rank_diff = to_rank - from_rank;
            file_diff == rank_diff && file_diff > 0
        }
        Direction::DownRight => {
            let file_diff = to_file - from_file;
            let rank_diff = to_rank - from_rank;
            file_diff == -rank_diff && file_diff < 0
        }
    }
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
    use crate::types::{File, Rank};

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
}
