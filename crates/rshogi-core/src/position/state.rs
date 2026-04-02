//! 局面状態（StateInfo）
//!
//! Zobrist ハッシュや王手情報を保持する。
//! NNUE 差分更新用の Accumulator/DirtyPiece は AccumulatorStack に移動。

use super::zobrist::zobrist_no_pawns;
use crate::bitboard::Bitboard;
use crate::types::{Color, Hand, Move, Piece, PieceType, RepetitionState, Value};

/// check_squares の圧縮配列サイズ
///
/// 成小駒 4 種を Gold に統合、King を省略して 15 → 9 エントリ。
/// StateInfo のサイズを 96B 削減（432B → 336B）。
pub(crate) const CHECK_SQUARES_SIZE: usize = 9;

/// 圧縮インデックス定数（CHECK_SQ_INDEX テーブルと一致させること）
pub(crate) const CS_IDX_PAWN: usize = 0;
pub(crate) const CS_IDX_LANCE: usize = 1;
pub(crate) const CS_IDX_KNIGHT: usize = 2;
pub(crate) const CS_IDX_SILVER: usize = 3;
pub(crate) const CS_IDX_BISHOP: usize = 4;
pub(crate) const CS_IDX_ROOK: usize = 5;
pub(crate) const CS_IDX_GOLD: usize = 6; // Gold + 成小駒
pub(crate) const CS_IDX_HORSE: usize = 7;
pub(crate) const CS_IDX_DRAGON: usize = 8;

/// PieceType → check_squares 圧縮インデックスの変換テーブル
///
/// King(8) は常に EMPTY なので配列に含めない（アクセス時に None で処理）。
/// ProPawn(9)/ProLance(10)/ProKnight(11)/ProSilver(12) は Gold(7) と同一なのでインデックス 6 を共有。
/// King のスロットは u8::MAX（無効値）。直接参照禁止（check_sq_index で None を返す）。
const CHECK_SQ_INDEX: [u8; PieceType::NUM + 1] = [
    0,                   // 0: unused (PieceType は 1 始まり)
    CS_IDX_PAWN as u8,   // Pawn(1)
    CS_IDX_LANCE as u8,  // Lance(2)
    CS_IDX_KNIGHT as u8, // Knight(3)
    CS_IDX_SILVER as u8, // Silver(4)
    CS_IDX_BISHOP as u8, // Bishop(5)
    CS_IDX_ROOK as u8,   // Rook(6)
    CS_IDX_GOLD as u8,   // Gold(7)
    u8::MAX,             // King(8) → 無効（check_sq_index で None を返すため直接参照禁止）
    CS_IDX_GOLD as u8,   // ProPawn(9) → Gold
    CS_IDX_GOLD as u8,   // ProLance(10) → Gold
    CS_IDX_GOLD as u8,   // ProKnight(11) → Gold
    CS_IDX_GOLD as u8,   // ProSilver(12) → Gold
    CS_IDX_HORSE as u8,  // Horse(13)
    CS_IDX_DRAGON as u8, // Dragon(14)
];

/// PieceType を check_squares の圧縮インデックスに変換
///
/// King の場合は None を返す（check_squares に King は含まれない）。
#[inline]
pub(crate) fn check_sq_index(pt: PieceType) -> Option<usize> {
    if pt == PieceType::King {
        None
    } else {
        // SAFETY: pt は 1..=14、CHECK_SQ_INDEX の長さは PieceType::NUM+1=15。
        Some(unsafe { *CHECK_SQ_INDEX.get_unchecked(pt as usize) } as usize)
    }
}

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
    /// パス権残数（パック形式）
    /// 上位4bit: 先手のパス権 (0-15)
    /// 下位4bit: 後手のパス権 (0-15)
    /// デフォルト: 0（パス権なし＝通常将棋）
    pub pass_rights: u8,

    // === 再計算される部分 ===
    /// 盤面ハッシュ（手番込み）
    pub board_key: u64,
    /// 手駒ハッシュ
    pub hand_key: u64,
    /// 手駒スナップショット（千日手判定用）
    pub hand_snapshot: [Hand; Color::NUM],
    /// 前の局面のインデックス（StateInfoプール内）
    pub previous: usize,
    /// 王手している駒
    pub checkers: Bitboard,
    /// pin駒 [Color]（自玉へのピン）
    pub blockers_for_king: [Bitboard; Color::NUM],
    /// pinしている駒 [Color]
    pub pinners: [Bitboard; Color::NUM],
    /// 王手となる升（圧縮配列）
    ///
    /// 成小駒（ProPawn, ProLance, ProKnight, ProSilver）は Gold と同一なので
    /// 統合し、King は常に EMPTY なので省略。15 → 9 エントリに削減。
    /// アクセスは `check_sq_index(pt)` でインデックス変換する。
    pub check_squares: [Bitboard; CHECK_SQUARES_SIZE],
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
    /// previous が未設定であることを表す sentinel
    pub const NO_PREVIOUS: usize = usize::MAX;

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
            pass_rights: 0, // パス権なし＝通常将棋
            board_key: 0,
            hand_key: 0,
            hand_snapshot: [Hand::EMPTY; Color::NUM],
            previous: Self::NO_PREVIOUS,
            checkers: Bitboard::EMPTY,
            blockers_for_king: [Bitboard::EMPTY; Color::NUM],
            pinners: [Bitboard::EMPTY; Color::NUM],
            check_squares: [Bitboard::EMPTY; CHECK_SQUARES_SIZE],
            captured_piece: Piece::NONE,
            repetition: 0,
            repetition_times: 0,
            repetition_type: RepetitionState::None,
            material_value: Value::ZERO,
            last_move: Move::NONE,
        }
    }

    /// 指定した手番のパス権残数を取得
    #[inline]
    pub fn get_pass_rights(&self, color: Color) -> u8 {
        match color {
            Color::Black => (self.pass_rights >> 4) & 0x0F,
            Color::White => self.pass_rights & 0x0F,
        }
    }

    /// 指定した手番のパス権を設定（内部用、Position経由で呼ぶこと）
    #[inline]
    pub(crate) fn set_pass_rights_internal(&mut self, color: Color, count: u8) {
        let count = count.min(15); // 15超は丸める
        match color {
            Color::Black => {
                self.pass_rights = (self.pass_rights & 0x0F) | ((count & 0x0F) << 4);
            }
            Color::White => {
                self.pass_rights = (self.pass_rights & 0xF0) | (count & 0x0F);
            }
        }
    }

    /// 局面のハッシュキー
    #[inline]
    pub fn key(&self) -> u64 {
        self.board_key ^ self.hand_key
    }

    /// 直前局面が存在するか
    #[inline]
    pub const fn has_previous(&self) -> bool {
        self.previous != Self::NO_PREVIOUS
    }

    /// 直前局面インデックスを取得
    #[inline]
    pub const fn previous_index(&self) -> Option<usize> {
        if self.has_previous() {
            Some(self.previous)
        } else {
            None
        }
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
            pass_rights: self.pass_rights, // パス権をコピー
            // 以下は再計算される
            board_key: self.board_key,
            hand_key: self.hand_key,
            hand_snapshot: self.hand_snapshot,
            previous: Self::NO_PREVIOUS,
            checkers: Bitboard::EMPTY,
            blockers_for_king: [Bitboard::EMPTY; Color::NUM],
            pinners: [Bitboard::EMPTY; Color::NUM],
            check_squares: [Bitboard::EMPTY; CHECK_SQUARES_SIZE],
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
        assert_eq!(state.previous, StateInfo::NO_PREVIOUS);
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
        assert_eq!(cloned.previous, StateInfo::NO_PREVIOUS);
    }

    // =========================================
    // パス権関連のテスト
    // =========================================

    #[test]
    fn test_pass_rights_storage() {
        let mut state = StateInfo::new();
        state.set_pass_rights_internal(Color::Black, 2);
        state.set_pass_rights_internal(Color::White, 3);
        assert_eq!(state.get_pass_rights(Color::Black), 2);
        assert_eq!(state.get_pass_rights(Color::White), 3);
        assert_eq!(state.pass_rights, 0x23);
    }

    #[test]
    fn test_pass_rights_clamp() {
        let mut state = StateInfo::new();
        state.set_pass_rights_internal(Color::Black, 20);
        assert_eq!(state.get_pass_rights(Color::Black), 15);
    }

    #[test]
    fn test_pass_rights_independence() {
        let mut state = StateInfo::new();
        // 先手のパス権を設定
        state.set_pass_rights_internal(Color::Black, 5);
        assert_eq!(state.get_pass_rights(Color::Black), 5);
        assert_eq!(state.get_pass_rights(Color::White), 0);

        // 後手のパス権を設定（先手は変わらない）
        state.set_pass_rights_internal(Color::White, 7);
        assert_eq!(state.get_pass_rights(Color::Black), 5);
        assert_eq!(state.get_pass_rights(Color::White), 7);
    }

    #[test]
    fn test_pass_rights_partial_clone() {
        let mut state = StateInfo::new();
        state.set_pass_rights_internal(Color::Black, 3);
        state.set_pass_rights_internal(Color::White, 5);

        let cloned = state.partial_clone();
        assert_eq!(cloned.get_pass_rights(Color::Black), 3);
        assert_eq!(cloned.get_pass_rights(Color::White), 5);
    }
}
