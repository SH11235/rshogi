//! Zobristハッシュ

use crate::types::{Color, Piece, PieceType, Square};
use std::sync::LazyLock;

/// Zobristハッシュ用乱数テーブル
pub struct Zobrist {
    /// 手番用
    pub side: u64,
    /// 駒×升 [Piece.index()][Square.index()]（盤外番兵分を+1確保）
    pub psq: [[u64; Square::NUM + 1]; 32],
    /// 手駒（加算型）[Color][PieceType index] — index 0は未使用
    pub hand: [[u64; 8]; Color::NUM],
    /// 盤上に歩が一枚もない時のキー
    pub no_pawns: u64,
}

impl Zobrist {
    /// テーブル初期化（YaneuraOu準拠のPRNGと生成順序）
    pub const fn init() -> Self {
        let mut zobrist = Zobrist {
            side: 0,
            psq: [[0; Square::NUM + 1]; 32],
            hand: [[0; 8]; Color::NUM],
            no_pawns: 0,
        };

        // YaneuraOu準拠: seed = 20151225, Xorshift64*, 1キーあたり4回呼び出し
        let mut seed: u64 = 20151225;

        // 手番用
        let (s, key) = next_key(seed);
        seed = s;
        zobrist.side = key;

        // 無歩時のキー
        let (s, key) = next_key(seed);
        seed = s;
        zobrist.no_pawns = key;

        // 駒×升
        // pc == 0 (Piece::NONE) は常に0を保つためスキップ
        let mut pc = 1;
        while pc < 32 {
            let mut sq = 0;
            while sq < Square::NUM {
                let (s, key) = next_key(seed);
                seed = s;
                zobrist.psq[pc][sq] = key;
                sq += 1;
            }
            pc += 1;
        }

        // 手駒（PieceType raw値でインデックス、0はスキップ）
        let mut c = 0;
        while c < Color::NUM {
            let mut pr = 1;
            while pr < 8 {
                let (s, key) = next_key(seed);
                seed = s;
                zobrist.hand[c][pr] = key;
                pr += 1;
            }
            c += 1;
        }

        // 最後のseed値は未使用だがPRNG状態保持のため意図的に保持
        let _ = seed;

        zobrist
    }
}

/// Xorshift64* 疑似乱数生成（YaneuraOu準拠、const fn対応）
/// shifts: 12, 25, 27 + multiply by 2685821657736338717
const fn xorshift64_star(mut s: u64) -> (u64, u64) {
    s ^= s >> 12;
    s ^= s << 25;
    s ^= s >> 27;
    let value = s.wrapping_mul(2685821657736338717u64);
    (s, value)
}

/// 4回PRNGを呼び、r1を返す（YaneuraOu準拠: 64bitモードでも4回呼んでPRNG状態を進める）
const fn next_key(seed: u64) -> (u64, u64) {
    let (s, r1) = xorshift64_star(seed);
    let (s, _) = xorshift64_star(s);
    let (s, _) = xorshift64_star(s);
    let (s, _) = xorshift64_star(s);
    (s, r1)
}

/// XorShift64疑似乱数生成（パス権用、const fn対応）
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
    ZOBRIST.hand[color.index()][pt.index()]
}

/// 手番のハッシュを取得
#[inline]
pub fn zobrist_side() -> u64 {
    ZOBRIST.side
}

// =============================================================================
// パス権用Zobristキー
// =============================================================================

/// パス権用Zobristキーのシード値
/// 既存のZobristキーと衝突しない値を選択
/// 注: 16進リテラルは 0-9, A-F のみ有効
const PASS_RIGHTS_ZOBRIST_SEED: u64 = 0x5A55_0000_0000_0001;

/// パス権用のZobristキー
/// [先手パス権0-15][後手パス権0-15] = 256エントリ
///
/// 【重要】(0,0) は 0 を維持し、通常ルールとのキー互換を保つ
static PASS_RIGHTS_KEYS: LazyLock<[[u64; 16]; 16]> = LazyLock::new(|| {
    let mut keys = [[0u64; 16]; 16];
    let mut seed = PASS_RIGHTS_ZOBRIST_SEED;

    for (black, row) in keys.iter_mut().enumerate() {
        for (white, key) in row.iter_mut().enumerate() {
            // (0,0) は 0 のまま → 通常ルールとキー互換
            if black == 0 && white == 0 {
                continue;
            }
            seed = xorshift64(seed);
            *key = seed;
        }
    }
    keys
});

/// パス権のZobristキーを取得
#[inline]
pub fn zobrist_pass_rights(black_rights: u8, white_rights: u8) -> u64 {
    PASS_RIGHTS_KEYS[black_rights.min(15) as usize][white_rights.min(15) as usize]
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

    // =========================================
    // パス権用Zobristキーのテスト
    // =========================================

    #[test]
    fn test_zobrist_pass_rights_zero_compatible() {
        // (0,0) は 0 → 通常ルールとキー互換
        assert_eq!(zobrist_pass_rights(0, 0), 0);
    }

    #[test]
    fn test_zobrist_pass_rights_nonzero() {
        // (0,0) 以外は非ゼロ（高確率で）
        // 注: RNGが偶然 0 を返す可能性は極めて低い
        assert_ne!(zobrist_pass_rights(1, 0), 0);
        assert_ne!(zobrist_pass_rights(0, 1), 0);
        assert_ne!(zobrist_pass_rights(2, 2), 0);
        assert_ne!(zobrist_pass_rights(15, 15), 0);
    }

    #[test]
    fn test_zobrist_pass_rights_uniqueness() {
        // 異なるパス権の組み合わせで異なるキー
        assert_ne!(zobrist_pass_rights(1, 0), zobrist_pass_rights(0, 1));
        assert_ne!(zobrist_pass_rights(2, 2), zobrist_pass_rights(3, 3));
        assert_ne!(zobrist_pass_rights(1, 1), zobrist_pass_rights(2, 2));
    }

    #[test]
    fn test_zobrist_pass_rights_clamp() {
        // 15を超える値は15に丸められる
        let key15 = zobrist_pass_rights(15, 15);
        let key20 = zobrist_pass_rights(20, 20);
        assert_eq!(key15, key20);
    }

    #[test]
    fn test_zobrist_pass_rights_xor_property() {
        // XOR性質: A ^ B ^ B = A
        let key1 = zobrist_pass_rights(2, 2);
        let key2 = zobrist_pass_rights(3, 3);

        let combined = key1 ^ key2;
        assert_eq!(combined ^ key2, key1);
        assert_eq!(combined ^ key1, key2);
    }
}
