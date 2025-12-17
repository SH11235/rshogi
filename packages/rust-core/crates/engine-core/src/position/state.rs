//! 局面状態（StateInfo）
//!
//! Zobrist ハッシュや王手情報に加えて、NNUE 差分更新用の Accumulator/DirtyPiece を保持する。

use super::zobrist::zobrist_no_pawns;
use crate::bitboard::Bitboard;
use crate::nnue::Accumulator;
use crate::types::{Color, Hand, Move, Piece, PieceType, RepetitionState, Square, Value};

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
    /// NNUE Accumulator（差分更新用の中間表現）
    pub accumulator: Accumulator,
    /// 差分更新用の駒移動情報
    pub dirty_piece: DirtyPiece,
}

/// 差分更新用の駒移動情報（固定長バッファでヒープ確保を回避）
#[derive(Clone, Copy)]
pub struct DirtyPiece {
    /// 変化した駒（最大3つ: 動いた駒 + 取られた駒）
    pieces: [ChangedPiece; Self::MAX_PIECES],
    /// 有効な pieces 要素数
    pieces_len: u8,
    /// 手駒の変化（最大2つ: 打ち駒 or 取り駒による変化）
    hand_changes: [HandChange; Self::MAX_HAND_CHANGES],
    /// 有効な hand_changes 要素数
    hand_changes_len: u8,
    /// 玉が動いたかどうか [Color]
    pub king_moved: [bool; Color::NUM],
}

impl DirtyPiece {
    /// pieces の最大要素数
    pub const MAX_PIECES: usize = 3;
    /// hand_changes の最大要素数
    pub const MAX_HAND_CHANGES: usize = 2;

    /// 新しい DirtyPiece を作成
    #[inline]
    pub const fn new() -> Self {
        Self {
            pieces: [ChangedPiece::EMPTY; Self::MAX_PIECES],
            pieces_len: 0,
            hand_changes: [HandChange::EMPTY; Self::MAX_HAND_CHANGES],
            hand_changes_len: 0,
            king_moved: [false; Color::NUM],
        }
    }

    /// 情報をクリア
    #[inline]
    pub fn clear(&mut self) {
        self.pieces_len = 0;
        self.hand_changes_len = 0;
        self.king_moved = [false; Color::NUM];
    }

    /// 駒変化を追加
    #[inline]
    pub fn push_piece(&mut self, piece: ChangedPiece) {
        let idx = self.pieces_len as usize;
        self.pieces[idx] = piece;
        self.pieces_len += 1;
    }

    /// 手駒変化を追加
    #[inline]
    pub fn push_hand_change(&mut self, change: HandChange) {
        let idx = self.hand_changes_len as usize;
        self.hand_changes[idx] = change;
        self.hand_changes_len += 1;
    }

    /// 駒変化のスライスを取得
    #[inline]
    pub fn pieces(&self) -> &[ChangedPiece] {
        &self.pieces[..self.pieces_len as usize]
    }

    /// 手駒変化のスライスを取得
    #[inline]
    pub fn hand_changes(&self) -> &[HandChange] {
        &self.hand_changes[..self.hand_changes_len as usize]
    }
}

impl Default for DirtyPiece {
    fn default() -> Self {
        Self::new()
    }
}

/// 1 駒分の変更情報
#[derive(Clone, Copy)]
pub struct ChangedPiece {
    /// 駒の色
    pub color: Color,
    /// 変更前の駒（盤上に無ければ Piece::NONE）
    pub old_piece: Piece,
    /// 変更前の位置（盤上に無ければ None）
    pub old_sq: Option<Square>,
    /// 変更後の駒（盤上に無ければ Piece::NONE）
    pub new_piece: Piece,
    /// 変更後の位置（盤上に無ければ None）
    pub new_sq: Option<Square>,
}

impl ChangedPiece {
    /// 空の ChangedPiece（固定長配列の初期化用）
    pub const EMPTY: Self = Self {
        color: Color::Black,
        old_piece: Piece::NONE,
        old_sq: None,
        new_piece: Piece::NONE,
        new_sq: None,
    };
}

/// 手駒の変化情報
#[derive(Clone, Copy)]
pub struct HandChange {
    pub owner: Color,
    pub piece_type: PieceType,
    pub old_count: u8,
    pub new_count: u8,
}

impl HandChange {
    /// 空の HandChange（固定長配列の初期化用）
    pub const EMPTY: Self = Self {
        owner: Color::Black,
        piece_type: PieceType::Pawn,
        old_count: 0,
        new_count: 0,
    };
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
            accumulator: Accumulator::new(),
            dirty_piece: DirtyPiece::default(),
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
            accumulator: Accumulator::new(),
            dirty_piece: DirtyPiece::default(),
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
