//! Board representation and basic manipulation
//!
//! This module contains the Board struct which manages piece placement using bitboards.

use super::bitboard::Bitboard;
use super::types::{Color, Piece, PieceType, Square};
#[cfg(debug_assertions)]
use log::warn;

/// Board representation
#[derive(Clone, Debug)]
pub struct Board {
    /// Bitboards by color and piece type [color][piece_type]
    /// - 目的: 駒種別・手番別の配置を管理
    /// - [手番(先手/後手)][駒種(8種類)]の2次元配列
    /// - 例: piece_bb[BLACK][PAWN] = 先手の歩の位置すべて
    /// - 用途: 特定の駒種の移動生成、駒の価値計算
    pub piece_bb: [[Bitboard; 8]; 2], // 8 piece types

    /// All pieces by color (cache)
    /// - 目的: 各手番の全駒位置をキャッシュ
    /// - occupied_bb[BLACK] = 先手の全駒のOR演算結果
    /// - 用途: 自分の駒への移動を除外、王手判定の高速化
    /// - 利点: 毎回piece_bbをOR演算する必要がない
    pub occupied_bb: [Bitboard; 2], // [color]
    /// - 目的: 盤上の全駒位置（両手番）をキャッシュ
    /// - occupied_bb[BLACK] | occupied_bb[WHITE]の結果
    /// - 用途: 空きマス判定、飛び駒の移動範囲計算
    /// - 利点: 最も頻繁に使用されるため事前計算
    /// - 更新タイミング:
    ///   1. 手を指した時 (make_moveメソッド)
    ///   2. 手を戻した時 (unmake_moveメソッド)
    ///   3. 局面を設定した時 (set_positionなど)
    pub all_bb: Bitboard,

    /// Promoted pieces bitboard
    /// - 目的: 成り駒の位置を記録
    /// - 成り駒かどうかの判定を高速化
    /// - 用途: 駒の動き生成時の成り判定、駒の表示
    /// - 利点: 駒種と成り状態を別管理することで効率化
    pub promoted_bb: Bitboard,

    /// Piece on each square (fast access)
    pub squares: [Option<Piece>; 81],
}

impl Board {
    /// Create empty board
    pub fn empty() -> Self {
        Board {
            piece_bb: [[Bitboard::EMPTY; 8]; 2],
            occupied_bb: [Bitboard::EMPTY; 2],
            all_bb: Bitboard::EMPTY,
            promoted_bb: Bitboard::EMPTY,
            squares: [None; 81],
        }
    }

    /// Place piece on board
    pub fn put_piece(&mut self, sq: Square, piece: Piece) {
        let color = piece.color as usize;
        let piece_type = piece.piece_type as usize;

        // Update bitboards
        self.piece_bb[color][piece_type].set(sq);
        self.occupied_bb[color].set(sq);
        self.all_bb.set(sq);

        // Update promoted bitboard
        if piece.promoted {
            self.promoted_bb.set(sq);
        }

        // Update square info
        self.squares[sq.index()] = Some(piece);
    }

    /// Remove piece from board
    pub fn remove_piece(&mut self, sq: Square) -> Option<Piece> {
        if let Some(piece) = self.squares[sq.index()] {
            let color = piece.color as usize;
            let piece_type = piece.piece_type as usize;

            // Update bitboards
            self.piece_bb[color][piece_type].clear(sq);
            self.occupied_bb[color].clear(sq);
            self.all_bb.clear(sq);

            // Update promoted bitboard
            if piece.promoted {
                self.promoted_bb.clear(sq);
            }

            // Clear square info
            self.squares[sq.index()] = None;

            Some(piece)
        } else {
            None
        }
    }

    /// Get piece on square
    #[inline]
    pub fn piece_on(&self, sq: Square) -> Option<Piece> {
        self.squares[sq.index()]
    }

    /// Rebuild occupancy bitboards from piece bitboards
    /// This is useful after manual bitboard manipulation (e.g., in tests)
    pub fn rebuild_occupancy_bitboards(&mut self) {
        // Clear existing occupancy bitboards
        self.all_bb = Bitboard::EMPTY;
        self.occupied_bb[0] = Bitboard::EMPTY;
        self.occupied_bb[1] = Bitboard::EMPTY;

        // Rebuild from piece bitboards
        for color in 0..2 {
            for piece_type in 0..8 {
                self.occupied_bb[color] |= self.piece_bb[color][piece_type];
            }
            self.all_bb |= self.occupied_bb[color];
        }
    }

    /// Get pieces of specific type and color
    pub fn pieces_of_type_and_color(&self, piece_type: PieceType, color: Color) -> Bitboard {
        self.piece_bb[color as usize][piece_type as usize]
    }

    /// Find king square
    pub fn king_square(&self, color: Color) -> Option<Square> {
        let mut bb = self.piece_bb[color as usize][PieceType::King as usize];
        let king_sq = bb.pop_lsb();

        #[cfg(debug_assertions)]
        {
            if king_sq.is_none() {
                warn!("No king found for {color:?}");
                warn!("Board state: all_bb has {} pieces", self.all_bb.count_ones());
            }
            // Verify there's only one king
            if !bb.is_empty() {
                panic!("Multiple kings found for {color:?}");
            }
        }

        king_sq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usi::parse_usi_square;

    #[test]
    fn test_board_operations() {
        let mut board = Board::empty();
        let sq = parse_usi_square("5e").unwrap();
        let piece = Piece::new(PieceType::Pawn, Color::Black);

        board.put_piece(sq, piece);
        assert_eq!(board.piece_on(sq), Some(piece));
        assert!(board.all_bb.test(sq));

        board.remove_piece(sq);
        assert_eq!(board.piece_on(sq), None);
        assert!(!board.all_bb.test(sq));
    }

    #[test]
    fn test_king_square_edge_cases() {
        // 空の盤面（玉がない）
        let mut board = Board::empty();
        assert_eq!(board.king_square(Color::Black), None);
        assert_eq!(board.king_square(Color::White), None);

        // 玉を配置
        let black_king = Piece::new(PieceType::King, Color::Black);
        let white_king = Piece::new(PieceType::King, Color::White);

        board.put_piece(parse_usi_square("5a").unwrap(), black_king);
        board.put_piece(parse_usi_square("5i").unwrap(), white_king);

        assert_eq!(board.king_square(Color::Black), Some(parse_usi_square("5a").unwrap()));
        assert_eq!(board.king_square(Color::White), Some(parse_usi_square("5i").unwrap()));

        // 玉を移動
        board.remove_piece(parse_usi_square("5a").unwrap());
        board.put_piece(parse_usi_square("4b").unwrap(), black_king);

        assert_eq!(board.king_square(Color::Black), Some(parse_usi_square("4b").unwrap()));
        assert_eq!(board.king_square(Color::White), Some(parse_usi_square("5i").unwrap()));
    }
}
