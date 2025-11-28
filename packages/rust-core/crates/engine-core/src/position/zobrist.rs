//! Zobristハッシュ

use crate::types::{Color, Piece, PieceType, Square};

/// Zobristハッシュ用乱数テーブル
pub struct Zobrist {
    /// 手番用
    pub side: u64,
    /// 駒×升 [Piece.index()][Square.index()]
    pub psq: [[u64; Square::NUM]; 32],
    /// 手駒（加算型）[Color][PieceType]
    pub hand: [[u64; 8]; Color::NUM],
}

impl Zobrist {
    /// テーブル初期化
    pub const fn init() -> Self {
        let mut zobrist = Zobrist {
            side: 0,
            psq: [[0; Square::NUM]; 32],
            hand: [[0; 8]; Color::NUM],
        };

        // XorShift64で疑似乱数生成
        let mut seed = 0x123456789ABCDEF0u64;

        // 手番用
        seed = xorshift64(seed);
        zobrist.side = seed;

        // 駒×升
        let mut pc = 0;
        while pc < 32 {
            let mut sq = 0;
            while sq < Square::NUM {
                seed = xorshift64(seed);
                zobrist.psq[pc][sq] = seed;
                sq += 1;
            }
            pc += 1;
        }

        // 手駒
        let mut c = 0;
        while c < Color::NUM {
            let mut pt = 0;
            while pt < 8 {
                seed = xorshift64(seed);
                zobrist.hand[c][pt] = seed;
                pt += 1;
            }
            c += 1;
        }

        zobrist
    }
}

/// XorShift64疑似乱数生成（const fn対応）
const fn xorshift64(mut x: u64) -> u64 {
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

/// グローバルZobristテーブル
pub static ZOBRIST: Zobrist = Zobrist::init();

/// 駒と升のハッシュを取得
#[inline]
pub fn zobrist_psq(pc: Piece, sq: Square) -> u64 {
    ZOBRIST.psq[pc.index()][sq.index()]
}

/// 手駒のハッシュを取得
#[inline]
pub fn zobrist_hand(color: Color, pt: PieceType) -> u64 {
    ZOBRIST.hand[color.index()][pt as usize]
}

/// 手番のハッシュを取得
#[inline]
pub fn zobrist_side() -> u64 {
    ZOBRIST.side
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_zobrist_init() {
        // 初期化が正常に完了していることを確認
        assert_ne!(ZOBRIST.side, 0);

        // 異なる駒・升の組み合わせで異なるハッシュ値
        let sq11 = Square::new(File::File1, Rank::Rank1);
        let sq12 = Square::new(File::File1, Rank::Rank2);
        assert_ne!(zobrist_psq(Piece::B_PAWN, sq11), zobrist_psq(Piece::B_PAWN, sq12));
        assert_ne!(zobrist_psq(Piece::B_PAWN, sq11), zobrist_psq(Piece::W_PAWN, sq11));
    }

    #[test]
    fn test_zobrist_hand() {
        // 異なる手駒で異なるハッシュ値
        assert_ne!(
            zobrist_hand(Color::Black, PieceType::Pawn),
            zobrist_hand(Color::Black, PieceType::Lance)
        );
        assert_ne!(
            zobrist_hand(Color::Black, PieceType::Pawn),
            zobrist_hand(Color::White, PieceType::Pawn)
        );
    }

    #[test]
    fn test_zobrist_xor_property() {
        // XOR性質: A ^ B ^ B = A
        let sq = Square::new(File::File5, Rank::Rank5);
        let h1 = zobrist_psq(Piece::B_PAWN, sq);
        let h2 = zobrist_psq(Piece::B_GOLD, sq);

        let combined = h1 ^ h2;
        assert_eq!(combined ^ h2, h1);
        assert_eq!(combined ^ h1, h2);
    }
}
