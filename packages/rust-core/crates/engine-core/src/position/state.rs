//! 局面状態（StateInfo）

use crate::bitboard::Bitboard;
use crate::types::{Color, Move, Piece, PieceType, Value};

/// 局面状態
///
/// do_move時に前の状態を保存し、undo_move時に復元するための情報を保持する。
#[derive(Clone)]
pub struct StateInfo {
    // === do_move時にコピーされる部分 ===
    /// 駒割ハッシュ
    pub material_key: u64,
    /// 歩のハッシュ（打ち歩詰め判定用）
    pub pawn_key: u64,
    /// null moveからの手数
    pub plies_from_null: i32,
    /// 連続王手カウンタ [Color]
    pub continuous_check: [i32; Color::NUM],

    // === 再計算される部分 ===
    /// 盤面ハッシュ（手番込み）
    pub board_key: u64,
    /// 手駒ハッシュ
    pub hand_key: u64,
    /// 王手している駒
    pub checkers: Bitboard,
    /// 前の局面へのポインタ
    pub previous: Option<Box<StateInfo>>,
    /// pin駒 [Color]（自玉へのピン）
    pub blockers_for_king: [Bitboard; Color::NUM],
    /// pinしている駒 [Color]
    pub pinners: [Bitboard; Color::NUM],
    /// 王手となる升 [PieceType]
    pub check_squares: [Bitboard; PieceType::NUM + 1],
    /// 捕獲した駒
    pub captured_piece: Piece,
    /// 千日手判定用カウンタ
    pub repetition: i32,
    /// 駒割評価値
    pub material_value: Value,
    /// 直前の指し手
    pub last_move: Move,
}

impl StateInfo {
    /// 空の状態を生成
    pub fn new() -> Self {
        StateInfo {
            material_key: 0,
            pawn_key: 0,
            plies_from_null: 0,
            continuous_check: [0; Color::NUM],
            board_key: 0,
            hand_key: 0,
            checkers: Bitboard::EMPTY,
            previous: None,
            blockers_for_king: [Bitboard::EMPTY; Color::NUM],
            pinners: [Bitboard::EMPTY; Color::NUM],
            check_squares: [Bitboard::EMPTY; PieceType::NUM + 1],
            captured_piece: Piece::NONE,
            repetition: 0,
            material_value: Value::ZERO,
            last_move: Move::NONE,
        }
    }

    /// 局面のハッシュキー
    #[inline]
    pub fn key(&self) -> u64 {
        self.board_key ^ self.hand_key
    }

    /// do_move用に部分コピー
    pub fn partial_clone(&self) -> Self {
        StateInfo {
            material_key: self.material_key,
            pawn_key: self.pawn_key,
            plies_from_null: self.plies_from_null,
            continuous_check: self.continuous_check,
            // 以下は再計算される
            board_key: self.board_key,
            hand_key: self.hand_key,
            checkers: Bitboard::EMPTY,
            previous: None,
            blockers_for_king: [Bitboard::EMPTY; Color::NUM],
            pinners: [Bitboard::EMPTY; Color::NUM],
            check_squares: [Bitboard::EMPTY; PieceType::NUM + 1],
            captured_piece: Piece::NONE,
            repetition: 0,
            material_value: self.material_value,
            last_move: Move::NONE,
        }
    }
}

impl Default for StateInfo {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_info_new() {
        let state = StateInfo::new();
        assert_eq!(state.board_key, 0);
        assert_eq!(state.hand_key, 0);
        assert_eq!(state.key(), 0);
        assert!(state.checkers.is_empty());
        assert!(state.previous.is_none());
    }

    #[test]
    fn test_state_info_key() {
        let mut state = StateInfo::new();
        state.board_key = 0x1234;
        state.hand_key = 0x5678;
        assert_eq!(state.key(), 0x1234 ^ 0x5678);
    }

    #[test]
    fn test_state_info_partial_clone() {
        let mut state = StateInfo::new();
        state.material_key = 100;
        state.plies_from_null = 5;
        state.continuous_check = [3, 2];

        let cloned = state.partial_clone();
        assert_eq!(cloned.material_key, 100);
        assert_eq!(cloned.plies_from_null, 5);
        assert_eq!(cloned.continuous_check, [3, 2]);
        assert!(cloned.previous.is_none());
    }
}
