//! BonaPiece for HalfKA - Non-mirror版
//!
//! HalfKAアーキテクチャ用のBonaPiece定義。
//! nnue-pytorchの実装に準拠し、以下の特徴を持つ:
//! - キング位置: 81マス直指定（ミラーなし）
//! - 入力平面数: 1710（1548 + 81 * 2）
//!
//! 注意: HalfKA^（factorized）は学習時のみの分解。
//! 推論時はHalfKAのBase特徴量のみで計算する。

use crate::types::{Color, Square};

// =============================================================================
// 定数定義（YaneuraOu evaluate.h準拠、DISTINGUISH_GOLDS無効）
// =============================================================================

// HalfKAのBonaPieceレイアウト参照用の定数（ロジックでは直接使わない）
// const FE_HAND_END: usize = 90;
// const FE_OLD_END: usize = 1548; // e_dragon + 81 = 1467 + 81

/// HalfKAの入力平面数（1548 + 81 * 2 = 1710）
pub const PIECE_INPUTS: usize = 1548 + 81 * 2;

// =============================================================================
// HalfKAインデックス計算（Non-mirror）
// =============================================================================

/// King位置を81マス直指定で取得（視点に応じて反転）
#[inline]
pub fn king_index(ksq: Square, perspective: Color) -> usize {
    if perspective == Color::Black {
        ksq.index()
    } else {
        ksq.inverse().index()
    }
}

/// HalfKAの特徴インデックスを計算
///
/// C++実装:
/// ```cpp
/// return fe_end2 * king_sq + bonapiece;
/// ```
#[inline]
pub fn halfka_index(king_idx: usize, bp_value: usize) -> usize {
    king_idx * PIECE_INPUTS + bp_value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_constants() {
        assert_eq!(PIECE_INPUTS, 1710);
    }

    #[test]
    fn test_king_index_black_perspective() {
        let sq_59 = Square::new(File::File5, Rank::Rank9);
        assert_eq!(king_index(sq_59, Color::Black), sq_59.index());
    }

    #[test]
    fn test_king_index_white_perspective() {
        let sq_59 = Square::new(File::File5, Rank::Rank9);
        let sq_51 = sq_59.inverse();
        assert_eq!(king_index(sq_59, Color::White), sq_51.index());
    }

    #[test]
    fn test_halfka_index() {
        assert_eq!(halfka_index(0, 0), 0);
        assert_eq!(halfka_index(1, 0), PIECE_INPUTS);
        assert_eq!(halfka_index(80, 0), 80 * PIECE_INPUTS);
    }
}
