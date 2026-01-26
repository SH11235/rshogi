//! 利きテーブルとBitboardマスク

use crate::types::{Color, File, PieceType, Rank, Square};

use super::Bitboard;

/// 筋のBitboard
pub static FILE_BB: [Bitboard; File::NUM] = init_file_bb();

/// 段のBitboard
pub static RANK_BB: [Bitboard; Rank::NUM] = init_rank_bb();

/// 各マスのBitboard
pub static SQUARE_BB: [Bitboard; Square::NUM] = init_square_bb();

/// 歩の利き [Color][Square]
pub static PAWN_EFFECT: [[Bitboard; Square::NUM]; Color::NUM] = init_pawn_effect();

/// 桂の利き [Color][Square]
pub static KNIGHT_EFFECT: [[Bitboard; Square::NUM]; Color::NUM] = init_knight_effect();

/// 銀の利き [Color][Square]
pub static SILVER_EFFECT: [[Bitboard; Square::NUM]; Color::NUM] = init_silver_effect();

/// 金の利き [Color][Square]
pub static GOLD_EFFECT: [[Bitboard; Square::NUM]; Color::NUM] = init_gold_effect();

/// 王の利き [Square]
pub static KING_EFFECT: [Bitboard; Square::NUM] = init_king_effect();

// === 初期化関数 ===

const fn init_file_bb() -> [Bitboard; File::NUM] {
    let mut result = [Bitboard::EMPTY; File::NUM];
    let mut file = 0u8;
    while file < 9 {
        let base = file as usize * 9;
        // 1筋〜7筋はp[0]、8-9筋はp[1]
        if file < 7 {
            let mut bits = 0u64;
            let mut rank = 0;
            while rank < 9 {
                bits |= 1u64 << (base + rank);
                rank += 1;
            }
            result[file as usize] = Bitboard::new(bits, 0);
        } else {
            let mut bits = 0u64;
            let mut rank = 0;
            while rank < 9 {
                bits |= 1u64 << (base - 63 + rank);
                rank += 1;
            }
            result[file as usize] = Bitboard::new(0, bits);
        }
        file += 1;
    }
    result
}

const fn init_rank_bb() -> [Bitboard; Rank::NUM] {
    let mut result = [Bitboard::EMPTY; Rank::NUM];
    let mut rank = 0u8;
    while rank < 9 {
        let mut p0 = 0u64;
        let mut p1 = 0u64;
        let mut file = 0u8;
        while file < 9 {
            let idx = file as usize * 9 + rank as usize;
            if idx < 63 {
                p0 |= 1u64 << idx;
            } else {
                p1 |= 1u64 << (idx - 63);
            }
            file += 1;
        }
        result[rank as usize] = Bitboard::new(p0, p1);
        rank += 1;
    }
    result
}

const fn init_square_bb() -> [Bitboard; Square::NUM] {
    let mut result = [Bitboard::EMPTY; Square::NUM];
    let mut i = 0;
    while i < 81 {
        if i < 63 {
            result[i] = Bitboard::new(1u64 << i, 0);
        } else {
            result[i] = Bitboard::new(0, 1u64 << (i - 63));
        }
        i += 1;
    }
    result
}

const fn init_pawn_effect() -> [[Bitboard; Square::NUM]; Color::NUM] {
    let mut result = [[Bitboard::EMPTY; Square::NUM]; Color::NUM];
    let mut sq = 0;
    while sq < 81 {
        let file = sq / 9;
        let rank = sq % 9;

        // 先手: 前方（rank - 1）
        if rank > 0 {
            let to = file * 9 + (rank - 1);
            result[0][sq] = square_bb_const(to);
        }

        // 後手: 後方（rank + 1）
        if rank < 8 {
            let to = file * 9 + (rank + 1);
            result[1][sq] = square_bb_const(to);
        }

        sq += 1;
    }
    result
}

const fn init_knight_effect() -> [[Bitboard; Square::NUM]; Color::NUM] {
    let mut result = [[Bitboard::EMPTY; Square::NUM]; Color::NUM];
    let mut sq = 0;
    while sq < 81 {
        let file = sq / 9;
        let rank = sq % 9;

        // 先手: 2マス前方、左右1マス
        if rank >= 2 {
            let to_rank = rank - 2;
            // 左（file + 1）
            if file < 8 {
                let to = (file + 1) * 9 + to_rank;
                result[0][sq] = bb_or_const(result[0][sq], square_bb_const(to));
            }
            // 右（file - 1）
            if file > 0 {
                let to = (file - 1) * 9 + to_rank;
                result[0][sq] = bb_or_const(result[0][sq], square_bb_const(to));
            }
        }

        // 後手: 2マス後方、左右1マス
        if rank <= 6 {
            let to_rank = rank + 2;
            // 左（file - 1 from white's perspective = file + 1 in absolute）
            if file < 8 {
                let to = (file + 1) * 9 + to_rank;
                result[1][sq] = bb_or_const(result[1][sq], square_bb_const(to));
            }
            // 右
            if file > 0 {
                let to = (file - 1) * 9 + to_rank;
                result[1][sq] = bb_or_const(result[1][sq], square_bb_const(to));
            }
        }

        sq += 1;
    }
    result
}

const fn init_silver_effect() -> [[Bitboard; Square::NUM]; Color::NUM] {
    let mut result = [[Bitboard::EMPTY; Square::NUM]; Color::NUM];
    let mut sq = 0;
    while sq < 81 {
        let file = sq / 9;
        let rank = sq % 9;

        // 先手銀: 前3方向 + 斜め後ろ2方向
        let mut bb_black = Bitboard::EMPTY;
        // 前
        if rank > 0 {
            bb_black = bb_or_const(bb_black, square_bb_const(file * 9 + (rank - 1)));
        }
        // 左前
        if file < 8 && rank > 0 {
            bb_black = bb_or_const(bb_black, square_bb_const((file + 1) * 9 + (rank - 1)));
        }
        // 右前
        if file > 0 && rank > 0 {
            bb_black = bb_or_const(bb_black, square_bb_const((file - 1) * 9 + (rank - 1)));
        }
        // 左後ろ
        if file < 8 && rank < 8 {
            bb_black = bb_or_const(bb_black, square_bb_const((file + 1) * 9 + (rank + 1)));
        }
        // 右後ろ
        if file > 0 && rank < 8 {
            bb_black = bb_or_const(bb_black, square_bb_const((file - 1) * 9 + (rank + 1)));
        }
        result[0][sq] = bb_black;

        // 後手銀: 180度回転
        let mut bb_white = Bitboard::EMPTY;
        // 後ろ
        if rank < 8 {
            bb_white = bb_or_const(bb_white, square_bb_const(file * 9 + (rank + 1)));
        }
        // 左後ろ（後手視点で前）
        if file < 8 && rank < 8 {
            bb_white = bb_or_const(bb_white, square_bb_const((file + 1) * 9 + (rank + 1)));
        }
        // 右後ろ（後手視点で前）
        if file > 0 && rank < 8 {
            bb_white = bb_or_const(bb_white, square_bb_const((file - 1) * 9 + (rank + 1)));
        }
        // 左前（後手視点で後ろ）
        if file < 8 && rank > 0 {
            bb_white = bb_or_const(bb_white, square_bb_const((file + 1) * 9 + (rank - 1)));
        }
        // 右前（後手視点で後ろ）
        if file > 0 && rank > 0 {
            bb_white = bb_or_const(bb_white, square_bb_const((file - 1) * 9 + (rank - 1)));
        }
        result[1][sq] = bb_white;

        sq += 1;
    }
    result
}

const fn init_gold_effect() -> [[Bitboard; Square::NUM]; Color::NUM] {
    let mut result = [[Bitboard::EMPTY; Square::NUM]; Color::NUM];
    let mut sq = 0;
    while sq < 81 {
        let file = sq / 9;
        let rank = sq % 9;

        // 先手金: 前3方向 + 左右 + 後ろ
        let mut bb_black = Bitboard::EMPTY;
        // 前
        if rank > 0 {
            bb_black = bb_or_const(bb_black, square_bb_const(file * 9 + (rank - 1)));
        }
        // 左前
        if file < 8 && rank > 0 {
            bb_black = bb_or_const(bb_black, square_bb_const((file + 1) * 9 + (rank - 1)));
        }
        // 右前
        if file > 0 && rank > 0 {
            bb_black = bb_or_const(bb_black, square_bb_const((file - 1) * 9 + (rank - 1)));
        }
        // 左
        if file < 8 {
            bb_black = bb_or_const(bb_black, square_bb_const((file + 1) * 9 + rank));
        }
        // 右
        if file > 0 {
            bb_black = bb_or_const(bb_black, square_bb_const((file - 1) * 9 + rank));
        }
        // 後ろ
        if rank < 8 {
            bb_black = bb_or_const(bb_black, square_bb_const(file * 9 + (rank + 1)));
        }
        result[0][sq] = bb_black;

        // 後手金: 180度回転
        let mut bb_white = Bitboard::EMPTY;
        // 後ろ（後手視点で前）
        if rank < 8 {
            bb_white = bb_or_const(bb_white, square_bb_const(file * 9 + (rank + 1)));
        }
        // 左後ろ
        if file < 8 && rank < 8 {
            bb_white = bb_or_const(bb_white, square_bb_const((file + 1) * 9 + (rank + 1)));
        }
        // 右後ろ
        if file > 0 && rank < 8 {
            bb_white = bb_or_const(bb_white, square_bb_const((file - 1) * 9 + (rank + 1)));
        }
        // 左
        if file < 8 {
            bb_white = bb_or_const(bb_white, square_bb_const((file + 1) * 9 + rank));
        }
        // 右
        if file > 0 {
            bb_white = bb_or_const(bb_white, square_bb_const((file - 1) * 9 + rank));
        }
        // 前（後手視点で後ろ）
        if rank > 0 {
            bb_white = bb_or_const(bb_white, square_bb_const(file * 9 + (rank - 1)));
        }
        result[1][sq] = bb_white;

        sq += 1;
    }
    result
}

const fn init_king_effect() -> [Bitboard; Square::NUM] {
    let mut result = [Bitboard::EMPTY; Square::NUM];
    let mut sq = 0;
    while sq < 81 {
        let file = sq / 9;
        let rank = sq % 9;

        let mut bb = Bitboard::EMPTY;

        // 8方向
        // 前
        if rank > 0 {
            bb = bb_or_const(bb, square_bb_const(file * 9 + (rank - 1)));
        }
        // 後ろ
        if rank < 8 {
            bb = bb_or_const(bb, square_bb_const(file * 9 + (rank + 1)));
        }
        // 左
        if file < 8 {
            bb = bb_or_const(bb, square_bb_const((file + 1) * 9 + rank));
        }
        // 右
        if file > 0 {
            bb = bb_or_const(bb, square_bb_const((file - 1) * 9 + rank));
        }
        // 左前
        if file < 8 && rank > 0 {
            bb = bb_or_const(bb, square_bb_const((file + 1) * 9 + (rank - 1)));
        }
        // 右前
        if file > 0 && rank > 0 {
            bb = bb_or_const(bb, square_bb_const((file - 1) * 9 + (rank - 1)));
        }
        // 左後ろ
        if file < 8 && rank < 8 {
            bb = bb_or_const(bb, square_bb_const((file + 1) * 9 + (rank + 1)));
        }
        // 右後ろ
        if file > 0 && rank < 8 {
            bb = bb_or_const(bb, square_bb_const((file - 1) * 9 + (rank + 1)));
        }

        result[sq] = bb;
        sq += 1;
    }
    result
}

// === ヘルパー関数（const fn用）===

const fn square_bb_const(sq: usize) -> Bitboard {
    if sq < 63 {
        Bitboard::new(1u64 << sq, 0)
    } else {
        Bitboard::new(0, 1u64 << (sq - 63))
    }
}

const fn bb_or_const(a: Bitboard, b: Bitboard) -> Bitboard {
    Bitboard::new(a.p0() | b.p0(), a.p1() | b.p1())
}

// === 利き取得関数 ===

/// 歩の利きを取得
#[inline]
pub fn pawn_effect(color: Color, sq: Square) -> Bitboard {
    PAWN_EFFECT[color.index()][sq.index()]
}

/// 桂の利きを取得
#[inline]
pub fn knight_effect(color: Color, sq: Square) -> Bitboard {
    KNIGHT_EFFECT[color.index()][sq.index()]
}

/// 銀の利きを取得
#[inline]
pub fn silver_effect(color: Color, sq: Square) -> Bitboard {
    SILVER_EFFECT[color.index()][sq.index()]
}

/// 金の利きを取得
#[inline]
pub fn gold_effect(color: Color, sq: Square) -> Bitboard {
    GOLD_EFFECT[color.index()][sq.index()]
}

/// 王の利きを取得
#[inline]
pub fn king_effect(sq: Square) -> Bitboard {
    KING_EFFECT[sq.index()]
}

/// 駒種に応じた利きを取得（近接駒のみ）
#[inline]
pub fn piece_effect(pt: PieceType, color: Color, sq: Square) -> Bitboard {
    match pt {
        PieceType::Pawn => pawn_effect(color, sq),
        PieceType::Knight => knight_effect(color, sq),
        PieceType::Silver => silver_effect(color, sq),
        PieceType::Gold
        | PieceType::ProPawn
        | PieceType::ProLance
        | PieceType::ProKnight
        | PieceType::ProSilver => gold_effect(color, sq),
        PieceType::King => king_effect(sq),
        // 遠方駒は別途occupiedを渡す必要あり
        _ => Bitboard::EMPTY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_bb() {
        // 1筋のBitboard
        let file1 = FILE_BB[0];
        assert_eq!(file1.count(), 9);
        for rank in 0..9 {
            let sq = Square::new(File::File1, Rank::from_u8(rank).unwrap());
            assert!(file1.contains(sq));
        }
        // 2筋以降には含まれない
        let sq21 = Square::new(File::File2, Rank::Rank1);
        assert!(!file1.contains(sq21));
    }

    #[test]
    fn test_rank_bb() {
        // 1段のBitboard
        let rank1 = RANK_BB[0];
        assert_eq!(rank1.count(), 9);
        for file in 0..9 {
            let sq = Square::new(File::from_u8(file).unwrap(), Rank::Rank1);
            assert!(rank1.contains(sq));
        }
        // 2段以降には含まれない
        let sq12 = Square::new(File::File1, Rank::Rank2);
        assert!(!rank1.contains(sq12));
    }

    #[test]
    fn test_pawn_effect() {
        // 先手5五の歩 -> 5四に利き
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq54 = Square::new(File::File5, Rank::Rank4);
        let bb = pawn_effect(Color::Black, sq55);
        assert_eq!(bb.count(), 1);
        assert!(bb.contains(sq54));

        // 後手5五の歩 -> 5六に利き
        let sq56 = Square::new(File::File5, Rank::Rank6);
        let bb = pawn_effect(Color::White, sq55);
        assert_eq!(bb.count(), 1);
        assert!(bb.contains(sq56));

        // 先手1一の歩 -> 利きなし（盤外）
        let sq11 = Square::new(File::File1, Rank::Rank1);
        let bb = pawn_effect(Color::Black, sq11);
        assert!(bb.is_empty());
    }

    #[test]
    fn test_knight_effect() {
        // 先手5五の桂 -> 4三、6三に利き
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq43 = Square::new(File::File4, Rank::Rank3);
        let sq63 = Square::new(File::File6, Rank::Rank3);
        let bb = knight_effect(Color::Black, sq55);
        assert_eq!(bb.count(), 2);
        assert!(bb.contains(sq43));
        assert!(bb.contains(sq63));

        // 後手5五の桂 -> 4七、6七に利き
        let sq47 = Square::new(File::File4, Rank::Rank7);
        let sq67 = Square::new(File::File6, Rank::Rank7);
        let bb = knight_effect(Color::White, sq55);
        assert_eq!(bb.count(), 2);
        assert!(bb.contains(sq47));
        assert!(bb.contains(sq67));
    }

    #[test]
    fn test_silver_effect() {
        // 先手5五の銀 -> 5方向に利き
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = silver_effect(Color::Black, sq55);
        assert_eq!(bb.count(), 5);

        // 前方3マス
        assert!(bb.contains(Square::new(File::File5, Rank::Rank4))); // 前
        assert!(bb.contains(Square::new(File::File4, Rank::Rank4))); // 右前
        assert!(bb.contains(Square::new(File::File6, Rank::Rank4))); // 左前
                                                                     // 斜め後ろ2マス
        assert!(bb.contains(Square::new(File::File4, Rank::Rank6))); // 右後ろ
        assert!(bb.contains(Square::new(File::File6, Rank::Rank6))); // 左後ろ
    }

    #[test]
    fn test_gold_effect() {
        // 先手5五の金 -> 6方向に利き
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = gold_effect(Color::Black, sq55);
        assert_eq!(bb.count(), 6);

        // 前方3マス
        assert!(bb.contains(Square::new(File::File5, Rank::Rank4))); // 前
        assert!(bb.contains(Square::new(File::File4, Rank::Rank4))); // 右前
        assert!(bb.contains(Square::new(File::File6, Rank::Rank4))); // 左前
                                                                     // 左右
        assert!(bb.contains(Square::new(File::File4, Rank::Rank5))); // 右
        assert!(bb.contains(Square::new(File::File6, Rank::Rank5))); // 左
                                                                     // 後ろ
        assert!(bb.contains(Square::new(File::File5, Rank::Rank6))); // 後ろ
    }

    #[test]
    fn test_king_effect() {
        // 5五の王 -> 8方向に利き
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = king_effect(sq55);
        assert_eq!(bb.count(), 8);

        // 隅の王 -> 3方向
        let sq11 = Square::new(File::File1, Rank::Rank1);
        let bb = king_effect(sq11);
        assert_eq!(bb.count(), 3);

        // 辺の王 -> 5方向
        let sq15 = Square::new(File::File1, Rank::Rank5);
        let bb = king_effect(sq15);
        assert_eq!(bb.count(), 5);
    }
}
