//! Zobristハッシュ

use crate::types::{Color, Piece, PieceType, Square};

/// Zobristハッシュ用乱数テーブル
pub struct Zobrist {
    /// 手番用
    pub side: u64,
    /// 駒×升 [Piece.index()][Square.index()]（盤外番兵分を+1確保）
    pub psq: [[u64; Square::NUM + 1]; 32],
    /// 手駒（加算型）[Color][手駒用PieceType]
    pub hand: [[u64; PieceType::HAND_NUM]; Color::NUM],
    /// 盤上に歩が一枚もない時のキー
    pub no_pawns: u64,
}

impl Zobrist {
    /// テーブル初期化
    pub const fn init() -> Self {
        let mut zobrist = Zobrist {
            side: 0,
            psq: [[0; Square::NUM + 1]; 32],
            hand: [[0; PieceType::HAND_NUM]; Color::NUM],
            no_pawns: 0,
        };

        // XorShift64で疑似乱数生成
        let mut seed = 0x123456789ABCDEF0u64;

        // 手番用
        seed = xorshift64(seed);
        zobrist.side = seed;

        // 無歩時のキー
        seed = xorshift64(seed);
        zobrist.no_pawns = seed;

        // 駒×升
        // pc == 0 (Piece::NONE) は常に0を保つためスキップ
        let mut pc = 1;
        while pc < 32 {
            let mut sq = 0;
            while sq < Square::NUM {
                seed = xorshift64(seed);
                zobrist.psq[pc][sq] = seed;
                sq += 1;
            }
            pc += 1;
        }

        // 手駒（HAND_NUMぶんのみ生成）
        let mut c = 0;
        while c < Color::NUM {
            let mut pt = 0;
            while pt < PieceType::HAND_NUM {
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

/// 盤上に歩が無い状態のハッシュを取得
#[inline]
pub fn zobrist_no_pawns() -> u64 {
    ZOBRIST.no_pawns
}

/// 手駒のハッシュを取得
#[inline]
pub fn zobrist_hand(color: Color, pt: PieceType) -> u64 {
    let idx = hand_index(pt).expect("zobrist_hand called with non-hand piece");
    ZOBRIST.hand[color.index()][idx]
}

const fn hand_index(pt: PieceType) -> Option<usize> {
    match pt {
        PieceType::Pawn => Some(0),
        PieceType::Lance => Some(1),
        PieceType::Knight => Some(2),
        PieceType::Silver => Some(3),
        PieceType::Gold => Some(4),
        PieceType::Bishop => Some(5),
        PieceType::Rook => Some(6),
        _ => None,
    }
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
