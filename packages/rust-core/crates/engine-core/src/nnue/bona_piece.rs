//! BonaPiece - 駒の種類と位置を一意に表現するインデックス
//!
//! YaneuraOu の NNUE 実装で用いられる BonaPiece に概ね準拠した定義。
//! - `PieceType` / 升 / 手番（視点）により一意なインデックスに写像する。
//! - 玉は特徴量に含めないため、BonaPiece としては常に `ZERO` を返す。
//! - 手駒は種類と枚数に応じて盤上特徴の末尾に割り当てる。

use crate::types::{Color, Piece, PieceType, Square};

/// fe_end: BonaPieceの最大値
///
/// YaneuraOu の HalfKP 用定義に基づく概算値。
/// 盤上駒 + 手駒の全パターン数で、おおよそ 1548 程度になる。
pub const FE_END: usize = 1548;

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
        // f_pawn = 1, e_pawn = 82, ...のようなオフセット
        let base = match pt {
            PieceType::Pawn => {
                if is_friend {
                    1
                } else {
                    82
                }
            }
            PieceType::Lance => {
                if is_friend {
                    163
                } else {
                    244
                }
            }
            PieceType::Knight => {
                if is_friend {
                    325
                } else {
                    406
                }
            }
            PieceType::Silver => {
                if is_friend {
                    487
                } else {
                    568
                }
            }
            PieceType::Gold
            | PieceType::ProPawn
            | PieceType::ProLance
            | PieceType::ProKnight
            | PieceType::ProSilver => {
                // 金と成駒（金の動き）は同じカテゴリ
                if is_friend {
                    649
                } else {
                    730
                }
            }
            PieceType::Bishop => {
                if is_friend {
                    811
                } else {
                    892
                }
            }
            PieceType::Rook => {
                if is_friend {
                    973
                } else {
                    1054
                }
            }
            PieceType::Horse => {
                if is_friend {
                    1135
                } else {
                    1216
                }
            }
            PieceType::Dragon => {
                if is_friend {
                    1297
                } else {
                    1378
                }
            }
            PieceType::King => {
                // 玉は特徴量に含めない
                return BonaPiece::ZERO;
            }
        };

        BonaPiece::new((base + sq_index) as u16)
    }

    /// 手駒からBonaPieceを計算
    ///
    /// 手駒は位置がないので、種類と枚数でインデックスを決定
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

        // 手駒のオフセット（盤上駒の後）
        // 実際の実装ではもっと複雑だが、簡略化
        let base = match pt {
            PieceType::Pawn => {
                if is_friend {
                    1459
                } else {
                    1477
                }
            }
            PieceType::Lance => {
                if is_friend {
                    1495
                } else {
                    1499
                }
            }
            PieceType::Knight => {
                if is_friend {
                    1503
                } else {
                    1507
                }
            }
            PieceType::Silver => {
                if is_friend {
                    1511
                } else {
                    1515
                }
            }
            PieceType::Gold => {
                if is_friend {
                    1519
                } else {
                    1523
                }
            }
            PieceType::Bishop => {
                if is_friend {
                    1527
                } else {
                    1529
                }
            }
            PieceType::Rook => {
                if is_friend {
                    1531
                } else {
                    1533
                }
            }
            _ => return BonaPiece::ZERO,
        };

        // countに応じてオフセット（0枚目は使わない）
        BonaPiece::new((base + count as usize - 1) as u16)
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
}
