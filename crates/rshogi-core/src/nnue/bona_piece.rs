//! BonaPiece - 駒の種類と位置を一意に表現するインデックス
//!
//! YaneuraOu の NNUE 実装で用いられる BonaPiece に準拠した定義。
//! - `PieceType` / 升 / 手番（視点）により一意なインデックスに写像する。
//! - 玉は特徴量に含めないため、BonaPiece としては常に `ZERO` を返す。
//!
//! ## YaneuraOu BonaPiece定義 (DISTINGUISH_GOLDS無効時)
//!
//! ### 手駒 (1〜89)
//! - f_hand_pawn = 1, e_hand_pawn = 20 (各18枚分)
//! - f_hand_lance = 39, e_hand_lance = 44 (各4枚分)
//! - f_hand_knight = 49, e_hand_knight = 54 (各4枚分)
//! - f_hand_silver = 59, e_hand_silver = 64 (各4枚分)
//! - f_hand_gold = 69, e_hand_gold = 74 (各4枚分)
//! - f_hand_bishop = 79, e_hand_bishop = 82 (各2枚分)
//! - f_hand_rook = 85, e_hand_rook = 88 (各2枚分)
//! - fe_hand_end = 90
//!
//! ### 盤上駒 (90〜1547)
//! - f_pawn = 90, e_pawn = 171
//! - f_lance = 252, e_lance = 333
//! - f_knight = 414, e_knight = 495
//! - f_silver = 576, e_silver = 657
//! - f_gold = 738, e_gold = 819
//! - f_bishop = 900, e_bishop = 981
//! - f_horse = 1062, e_horse = 1143
//! - f_rook = 1224, e_rook = 1305
//! - f_dragon = 1386, e_dragon = 1467
//! - fe_end = 1548

use crate::types::{Color, Piece, PieceType, Square};

// =============================================================================
// YaneuraOu形式の手駒BonaPiece定数
// =============================================================================

/// 手駒領域の終端
pub const FE_HAND_END: usize = 90;

// 先手の手駒ベースオフセット
pub const F_HAND_PAWN: u16 = 1;
pub const F_HAND_LANCE: u16 = 39;
pub const F_HAND_KNIGHT: u16 = 49;
pub const F_HAND_SILVER: u16 = 59;
pub const F_HAND_GOLD: u16 = 69;
pub const F_HAND_BISHOP: u16 = 79;
pub const F_HAND_ROOK: u16 = 85;

// 後手の手駒ベースオフセット
pub const E_HAND_PAWN: u16 = 20;
pub const E_HAND_LANCE: u16 = 44;
pub const E_HAND_KNIGHT: u16 = 54;
pub const E_HAND_SILVER: u16 = 64;
pub const E_HAND_GOLD: u16 = 74;
pub const E_HAND_BISHOP: u16 = 82;
pub const E_HAND_ROOK: u16 = 88;

// =============================================================================
// YaneuraOu形式の盤上駒BonaPiece定数
// =============================================================================

pub const F_PAWN: u16 = 90;
pub const E_PAWN: u16 = 171;
pub const F_LANCE: u16 = 252;
pub const E_LANCE: u16 = 333;
pub const F_KNIGHT: u16 = 414;
pub const E_KNIGHT: u16 = 495;
pub const F_SILVER: u16 = 576;
pub const E_SILVER: u16 = 657;
pub const F_GOLD: u16 = 738;
pub const E_GOLD: u16 = 819;
pub const F_BISHOP: u16 = 900;
pub const E_BISHOP: u16 = 981;
pub const F_HORSE: u16 = 1062;
pub const E_HORSE: u16 = 1143;
pub const F_ROOK: u16 = 1224;
pub const E_ROOK: u16 = 1305;
pub const F_DRAGON: u16 = 1386;
pub const E_DRAGON: u16 = 1467;

/// 駒種・is_friend に対する base offset テーブル（盤上駒用）
/// `[piece_type as usize][is_friend as usize]` -> base offset
/// is_friend: 0=enemy, 1=friend
///
/// PieceType は 1 始まり（Pawn=1, ..., Dragon=14）なので index 0 はダミー。
pub const PIECE_BASE: [[u16; 2]; 15] = [
    // index 0: 未使用（ダミー）
    [0, 0],
    // PieceType::Pawn = 1
    [E_PAWN, F_PAWN], // [enemy, friend]
    // PieceType::Lance = 2
    [E_LANCE, F_LANCE],
    // PieceType::Knight = 3
    [E_KNIGHT, F_KNIGHT],
    // PieceType::Silver = 4
    [E_SILVER, F_SILVER],
    // PieceType::Bishop = 5
    [E_BISHOP, F_BISHOP],
    // PieceType::Rook = 6
    [E_ROOK, F_ROOK],
    // PieceType::Gold = 7 (成駒と同じ)
    [E_GOLD, F_GOLD],
    // PieceType::King = 8 (使用しない、0埋め)
    [0, 0],
    // PieceType::ProPawn = 9 (Gold と同じ)
    [E_GOLD, F_GOLD],
    // PieceType::ProLance = 10 (Gold と同じ)
    [E_GOLD, F_GOLD],
    // PieceType::ProKnight = 11 (Gold と同じ)
    [E_GOLD, F_GOLD],
    // PieceType::ProSilver = 12 (Gold と同じ)
    [E_GOLD, F_GOLD],
    // PieceType::Horse = 13
    [E_HORSE, F_HORSE],
    // PieceType::Dragon = 14
    [E_DRAGON, F_DRAGON],
];

// コンパイル時の静的検証: PIECE_BASE テーブルの整合性チェック
// 手動定義のミスを防ぐため、各駒種のbaseオフセットが正しいことを検証
const _: () = {
    use crate::types::PieceType;

    // 生駒のチェック
    assert!(PIECE_BASE[PieceType::Pawn as usize][0] == E_PAWN);
    assert!(PIECE_BASE[PieceType::Pawn as usize][1] == F_PAWN);
    assert!(PIECE_BASE[PieceType::Lance as usize][0] == E_LANCE);
    assert!(PIECE_BASE[PieceType::Lance as usize][1] == F_LANCE);
    assert!(PIECE_BASE[PieceType::Knight as usize][0] == E_KNIGHT);
    assert!(PIECE_BASE[PieceType::Knight as usize][1] == F_KNIGHT);
    assert!(PIECE_BASE[PieceType::Silver as usize][0] == E_SILVER);
    assert!(PIECE_BASE[PieceType::Silver as usize][1] == F_SILVER);
    assert!(PIECE_BASE[PieceType::Bishop as usize][0] == E_BISHOP);
    assert!(PIECE_BASE[PieceType::Bishop as usize][1] == F_BISHOP);
    assert!(PIECE_BASE[PieceType::Rook as usize][0] == E_ROOK);
    assert!(PIECE_BASE[PieceType::Rook as usize][1] == F_ROOK);
    assert!(PIECE_BASE[PieceType::Gold as usize][0] == E_GOLD);
    assert!(PIECE_BASE[PieceType::Gold as usize][1] == F_GOLD);

    // 成駒のチェック（すべてGOLDと同じ扱い）
    assert!(PIECE_BASE[PieceType::ProPawn as usize][0] == E_GOLD);
    assert!(PIECE_BASE[PieceType::ProPawn as usize][1] == F_GOLD);
    assert!(PIECE_BASE[PieceType::ProLance as usize][0] == E_GOLD);
    assert!(PIECE_BASE[PieceType::ProLance as usize][1] == F_GOLD);
    assert!(PIECE_BASE[PieceType::ProKnight as usize][0] == E_GOLD);
    assert!(PIECE_BASE[PieceType::ProKnight as usize][1] == F_GOLD);
    assert!(PIECE_BASE[PieceType::ProSilver as usize][0] == E_GOLD);
    assert!(PIECE_BASE[PieceType::ProSilver as usize][1] == F_GOLD);

    // 馬・龍のチェック
    assert!(PIECE_BASE[PieceType::Horse as usize][0] == E_HORSE);
    assert!(PIECE_BASE[PieceType::Horse as usize][1] == F_HORSE);
    assert!(PIECE_BASE[PieceType::Dragon as usize][0] == E_DRAGON);
    assert!(PIECE_BASE[PieceType::Dragon as usize][1] == F_DRAGON);

    // Kingは使用しない（0埋め）
    assert!(PIECE_BASE[PieceType::King as usize][0] == 0);
    assert!(PIECE_BASE[PieceType::King as usize][1] == 0);
};

/// fe_end: BonaPieceの最大値
///
/// YaneuraOu の HalfKP 用定義に基づく。
/// fe_end = e_dragon + 81 = 1467 + 81 = 1548
pub const FE_END: usize = 1548;

// =============================================================================
// ExtBonaPiece - 先手視点/後手視点の BonaPiece ペア
// =============================================================================

/// 先手視点(fb)と後手視点(fw)の BonaPiece をペアで保持する構造体
///
/// YaneuraOu の ExtBonaPiece に相当。
/// PieceList の各エントリおよび DirtyPiece の変化情報に使用する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExtBonaPiece {
    /// 先手視点の BonaPiece
    pub fb: BonaPiece,
    /// 後手視点の BonaPiece
    pub fw: BonaPiece,
}

impl ExtBonaPiece {
    /// ゼロ値（無効）
    pub const ZERO: ExtBonaPiece = ExtBonaPiece {
        fb: BonaPiece::ZERO,
        fw: BonaPiece::ZERO,
    };

    /// 新しい ExtBonaPiece を作成
    #[inline]
    pub const fn new(fb: BonaPiece, fw: BonaPiece) -> Self {
        Self { fb, fw }
    }

    /// 盤上駒から ExtBonaPiece を生成
    ///
    /// Piece と Square から、先手/後手両視点の BonaPiece を計算する。
    /// King の場合は `bona_piece_halfka_hm` の定数を使用して King 用 BonaPiece を生成。
    #[inline]
    pub fn from_board(piece: Piece, sq: Square) -> Self {
        if piece.is_none() {
            return Self::ZERO;
        }
        let pt = piece.piece_type();
        let color = piece.color();

        if pt == PieceType::King {
            // King 用 BonaPiece: HalfKA_hm の F_KING / E_KING を使用
            use super::bona_piece_halfka_hm::{E_KING, F_KING};
            let (fb, fw) = if color == Color::Black {
                // 先手玉: fb = F_KING + sq, fw = E_KING + sq.inverse()
                (
                    BonaPiece::new(F_KING as u16 + sq.index() as u16),
                    BonaPiece::new(E_KING as u16 + sq.inverse().index() as u16),
                )
            } else {
                // 後手玉: fb = E_KING + sq, fw = F_KING + sq.inverse()
                (
                    BonaPiece::new(E_KING as u16 + sq.index() as u16),
                    BonaPiece::new(F_KING as u16 + sq.inverse().index() as u16),
                )
            };
            return Self { fb, fw };
        }

        let is_friend_black = (color == Color::Black) as usize;
        let is_friend_white = (color == Color::White) as usize;
        let base_fb = PIECE_BASE[pt as usize][is_friend_black];
        let base_fw = PIECE_BASE[pt as usize][is_friend_white];

        Self {
            fb: BonaPiece::new(base_fb + sq.index() as u16),
            fw: BonaPiece::new(base_fw + sq.inverse().index() as u16),
        }
    }

    /// 手駒から ExtBonaPiece を生成
    ///
    /// 手駒の (owner, pt, count) から先手/後手両視点の BonaPiece を計算する。
    #[inline]
    pub fn from_hand(owner: Color, pt: PieceType, count: u8) -> Self {
        if count == 0 {
            return Self::ZERO;
        }
        let fb = BonaPiece::from_hand_piece(Color::Black, owner, pt, count);
        let fw = BonaPiece::from_hand_piece(Color::White, owner, pt, count);
        Self { fb, fw }
    }
}

/// BonaPieceの定義
/// 駒の種類と位置を一意に表現するインデックス
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct BonaPiece(pub u16);

impl BonaPiece {
    /// ゼロ（無効値）
    pub const ZERO: BonaPiece = BonaPiece(0);

    /// 新しいBonaPieceを作成
    #[inline]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// 値を取得
    #[inline]
    pub const fn value(self) -> u16 {
        self.0
    }

    /// 盤上の駒からBonaPieceを計算
    ///
    /// YaneuraOuの定義に従う（evaluate.h参照）
    /// 視点（perspective）に応じて駒の位置とインデックスを変換
    pub fn from_piece_square(piece: Piece, sq: Square, perspective: Color) -> BonaPiece {
        if piece.is_none() {
            return BonaPiece::ZERO;
        }

        let pt = piece.piece_type();
        let pc_color = piece.color();

        // 視点に応じてマスを変換
        let sq_index = if perspective == Color::Black {
            sq.index()
        } else {
            sq.inverse().index()
        };

        // 駒の色が視点と同じかどうか
        let is_friend = pc_color == perspective;

        // 基本オフセット（YaneuraOuの定義に準拠）
        // 盤上駒は fe_hand_end (90) から始まる
        let base = match pt {
            PieceType::Pawn => {
                if is_friend {
                    F_PAWN
                } else {
                    E_PAWN
                }
            }
            PieceType::Lance => {
                if is_friend {
                    F_LANCE
                } else {
                    E_LANCE
                }
            }
            PieceType::Knight => {
                if is_friend {
                    F_KNIGHT
                } else {
                    E_KNIGHT
                }
            }
            PieceType::Silver => {
                if is_friend {
                    F_SILVER
                } else {
                    E_SILVER
                }
            }
            PieceType::Gold
            | PieceType::ProPawn
            | PieceType::ProLance
            | PieceType::ProKnight
            | PieceType::ProSilver => {
                // 金と成駒（金の動き）は同じカテゴリ
                if is_friend {
                    F_GOLD
                } else {
                    E_GOLD
                }
            }
            PieceType::Bishop => {
                if is_friend {
                    F_BISHOP
                } else {
                    E_BISHOP
                }
            }
            PieceType::Rook => {
                if is_friend {
                    F_ROOK
                } else {
                    E_ROOK
                }
            }
            PieceType::Horse => {
                if is_friend {
                    F_HORSE
                } else {
                    E_HORSE
                }
            }
            PieceType::Dragon => {
                if is_friend {
                    F_DRAGON
                } else {
                    E_DRAGON
                }
            }
            PieceType::King => {
                // 玉は特徴量に含めない
                return BonaPiece::ZERO;
            }
        };

        BonaPiece::new(base + sq_index as u16)
    }

    /// 手駒からBonaPieceを計算
    ///
    /// YaneuraOuの手駒BonaPiece定義に従う。
    /// 手駒は (駒種, 枚数) でインデックスが決まる。
    /// 枚数が増えると新しいBonaPieceになる（0→1枚でbase, 1→2枚でbase+1, ...）
    ///
    /// 注意: countは「現在の枚数」であり、「追加する枚数」ではない。
    /// count=1 のとき base が返る（1枚目のBonaPiece）。
    pub fn from_hand_piece(
        perspective: Color,
        owner: Color,
        pt: PieceType,
        count: u8,
    ) -> BonaPiece {
        if count == 0 {
            return BonaPiece::ZERO;
        }

        let is_friend = owner == perspective;

        // YaneuraOu形式の手駒オフセット（1から始まる）
        // 手駒は盤上駒より前に配置されている
        let base = match pt {
            PieceType::Pawn => {
                if is_friend {
                    F_HAND_PAWN
                } else {
                    E_HAND_PAWN
                }
            }
            PieceType::Lance => {
                if is_friend {
                    F_HAND_LANCE
                } else {
                    E_HAND_LANCE
                }
            }
            PieceType::Knight => {
                if is_friend {
                    F_HAND_KNIGHT
                } else {
                    E_HAND_KNIGHT
                }
            }
            PieceType::Silver => {
                if is_friend {
                    F_HAND_SILVER
                } else {
                    E_HAND_SILVER
                }
            }
            PieceType::Gold => {
                if is_friend {
                    F_HAND_GOLD
                } else {
                    E_HAND_GOLD
                }
            }
            PieceType::Bishop => {
                if is_friend {
                    F_HAND_BISHOP
                } else {
                    E_HAND_BISHOP
                }
            }
            PieceType::Rook => {
                if is_friend {
                    F_HAND_ROOK
                } else {
                    E_HAND_ROOK
                }
            }
            _ => return BonaPiece::ZERO,
        };

        // countに応じてオフセット
        // count=1 のとき base, count=2 のとき base+1, ...
        let bp = BonaPiece::new(base + count as u16 - 1);

        // 手駒のBonaPieceは必ずFE_HAND_END未満
        debug_assert!(
            (bp.0 as usize) < FE_HAND_END,
            "Hand piece BonaPiece {} exceeds FE_HAND_END {}",
            bp.0,
            FE_HAND_END
        );

        bp
    }
}

/// HalfKP特徴量のインデックスを計算
#[inline]
pub fn halfkp_index(king_sq: Square, bona_piece: BonaPiece) -> usize {
    king_sq.index() * FE_END + bona_piece.0 as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_bona_piece_zero() {
        assert_eq!(BonaPiece::ZERO.value(), 0);
    }

    #[test]
    fn test_bona_piece_from_piece_square() {
        let sq = Square::new(File::File7, Rank::Rank7);
        let piece = Piece::new(Color::Black, PieceType::Pawn);

        let bp = BonaPiece::from_piece_square(piece, sq, Color::Black);
        assert_ne!(bp, BonaPiece::ZERO);
    }

    #[test]
    fn test_bona_piece_king_returns_zero() {
        let sq = Square::new(File::File5, Rank::Rank9);
        let piece = Piece::new(Color::Black, PieceType::King);

        let bp = BonaPiece::from_piece_square(piece, sq, Color::Black);
        assert_eq!(bp, BonaPiece::ZERO);
    }

    #[test]
    fn test_halfkp_index() {
        let king_sq = Square::new(File::File5, Rank::Rank9);
        let bp = BonaPiece::new(100);

        let index = halfkp_index(king_sq, bp);
        assert_eq!(index, king_sq.index() * FE_END + 100);
    }

    #[test]
    fn test_piece_base_table_consistency() {
        // 全駒種について、PIECE_BASEテーブルとfrom_piece_square()の結果が一致することを確認
        let all_piece_types = [
            PieceType::Pawn,
            PieceType::Lance,
            PieceType::Knight,
            PieceType::Silver,
            PieceType::Gold,
            PieceType::Bishop,
            PieceType::Rook,
            PieceType::ProPawn,
            PieceType::ProLance,
            PieceType::ProKnight,
            PieceType::ProSilver,
            PieceType::Horse,
            PieceType::Dragon,
        ];

        let sq = Square::from_u8(0).unwrap();
        let perspective = Color::Black;

        for pt in all_piece_types {
            for &color in &[Color::Black, Color::White] {
                let is_friend = color == perspective;
                let piece = Piece::new(color, pt);

                // from_piece_square() の結果
                let bp_old = BonaPiece::from_piece_square(piece, sq, perspective);

                // PIECE_BASE テーブルからの結果
                let base = PIECE_BASE[pt as usize][is_friend as usize];
                let bp_new = BonaPiece::new(base);

                assert_eq!(
                    bp_old, bp_new,
                    "Mismatch for {:?}, color={:?}, is_friend={}",
                    pt, color, is_friend
                );
            }
        }
    }
}
