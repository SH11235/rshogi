//! 局面状態（StateInfo）
//!
//! Zobrist ハッシュや王手情報を保持する。
//! NNUE 差分更新用の Accumulator/DirtyPiece は AccumulatorStack に移動。

use super::zobrist::zobrist_no_pawns;
use crate::bitboard::Bitboard;
use crate::types::{Color, Hand, Move, Piece, PieceType, RepetitionState, Value};

/// 局面状態
///
/// do_move時に前の状態を保存し、undo_move時に復元するための情報を保持する。
/// NNUE関連（Accumulator/DirtyPiece）はSearchWorkerのAccumulatorStackで管理。
#[derive(Clone)]
pub struct StateInfo {
    // === do_move時にコピーされる部分 ===
    /// 駒割ハッシュ
    pub material_key: u64,
    /// 歩のハッシュ（打ち歩詰め判定用）
    pub pawn_key: u64,
    /// 小駒（香・桂・銀・金・その成り駒）のハッシュ
    pub minor_piece_key: u64,
    /// 歩以外の駒のハッシュ（手番別）
    pub non_pawn_key: [u64; Color::NUM],
    /// null moveからの手数
    pub plies_from_null: i32,
    /// 連続王手カウンタ [Color]
    pub continuous_check: [i32; Color::NUM],
    /// ゲーム開始からの総手数（320手ルール用）
    pub game_ply: u16,

    // === 再計算される部分 ===
    /// 盤面ハッシュ（手番込み）
    pub board_key: u64,
    /// 手駒ハッシュ
    pub hand_key: u64,
    /// 手駒スナップショット（千日手判定用）
    pub hand_snapshot: [Hand; Color::NUM],
    /// 王手している駒
    pub checkers: Bitboard,
    /// 前の局面のインデックス（StateInfoプール内）
    pub previous: Option<usize>,
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
    /// 千日手繰り返し回数
    pub repetition_times: i32,
    /// 千日手種別
    pub repetition_type: RepetitionState,
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
            pawn_key: zobrist_no_pawns(),
            minor_piece_key: 0,
            non_pawn_key: [0; Color::NUM],
            plies_from_null: 0,
            continuous_check: [0; Color::NUM],
            game_ply: 0,
            board_key: 0,
            hand_key: 0,
            hand_snapshot: [Hand::EMPTY; Color::NUM],
            checkers: Bitboard::EMPTY,
            previous: None,
            blockers_for_king: [Bitboard::EMPTY; Color::NUM],
            pinners: [Bitboard::EMPTY; Color::NUM],
            check_squares: [Bitboard::EMPTY; PieceType::NUM + 1],
            captured_piece: Piece::NONE,
            repetition: 0,
            repetition_times: 0,
            repetition_type: RepetitionState::None,
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
    ///
    /// NNUE関連（Accumulator/DirtyPiece）はAccumulatorStack側で管理するため、
    /// ここでは初期化しない。do_moveの主なコストはBitboard/ハッシュ更新のみになる。
    pub fn partial_clone(&self) -> Self {
        StateInfo {
            material_key: self.material_key,
            pawn_key: self.pawn_key,
            minor_piece_key: self.minor_piece_key,
            non_pawn_key: self.non_pawn_key,
            plies_from_null: self.plies_from_null,
            continuous_check: self.continuous_check,
            game_ply: self.game_ply,
            // 以下は再計算される
            board_key: self.board_key,
            hand_key: self.hand_key,
            hand_snapshot: self.hand_snapshot,
            checkers: Bitboard::EMPTY,
            previous: None,
            blockers_for_king: [Bitboard::EMPTY; Color::NUM],
            pinners: [Bitboard::EMPTY; Color::NUM],
            check_squares: [Bitboard::EMPTY; PieceType::NUM + 1],
            captured_piece: Piece::NONE,
            repetition: 0,
            repetition_times: 0,
            repetition_type: RepetitionState::None,
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
        assert_eq!(state.pawn_key, zobrist_no_pawns());
        assert_eq!(state.minor_piece_key, 0);
        assert_eq!(state.non_pawn_key, [0; Color::NUM]);
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
        state.minor_piece_key = 42;
        state.non_pawn_key = [7, 11];

        let cloned = state.partial_clone();
        assert_eq!(cloned.material_key, 100);
        assert_eq!(cloned.plies_from_null, 5);
        assert_eq!(cloned.continuous_check, [3, 2]);
        assert_eq!(cloned.minor_piece_key, 42);
        assert_eq!(cloned.non_pawn_key, [7, 11]);
        assert!(cloned.previous.is_none());
    }
}
