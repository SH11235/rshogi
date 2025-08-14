//! Pin information for SEE calculations
//!
//! This module provides lightweight pin detection functionality specifically
//! optimized for Static Exchange Evaluation (SEE).

use crate::shogi::board::{Bitboard, Square};

/// SEE用の軽量なピン情報
///
/// Tracks pinned pieces and their allowed movement directions during SEE calculation.
/// This is a simplified version of full pin detection, optimized for SEE performance.
#[derive(Debug, Clone)]
pub struct SeePinInfo {
    /// ピンされた駒のビットボード
    pub pinned: Bitboard,
    /// ピン方向のマスク（4方向）
    pub vertical_pins: Bitboard, // 縦方向のピン
    pub horizontal_pins: Bitboard, // 横方向のピン
    pub diag_ne_pins: Bitboard,    // 北東-南西の斜めピン
    pub diag_nw_pins: Bitboard,    // 北西-南東の斜めピン
}

impl SeePinInfo {
    /// 空のピン情報を作成
    pub fn empty() -> Self {
        SeePinInfo {
            pinned: Bitboard::EMPTY,
            vertical_pins: Bitboard::EMPTY,
            horizontal_pins: Bitboard::EMPTY,
            diag_ne_pins: Bitboard::EMPTY,
            diag_nw_pins: Bitboard::EMPTY,
        }
    }

    /// 指定された駒が指定された方向に移動できるかチェック
    ///
    /// # Arguments
    /// * `from` - 移動元のマス
    /// * `to` - 移動先のマス
    ///
    /// # Returns
    /// 移動可能な場合は`true`、ピンにより移動不可の場合は`false`
    pub fn can_move(&self, from: Square, to: Square) -> bool {
        // ピンされていない駒は自由に動ける
        if !self.pinned.test(from) {
            return true;
        }

        // ピンされている場合、ピンの方向に沿った移動のみ許可

        // 縦方向のピン
        if self.vertical_pins.test(from) {
            return from.file() == to.file();
        }

        // 横方向のピン
        if self.horizontal_pins.test(from) {
            return from.rank() == to.rank();
        }

        // 北東-南西の斜めピン
        if self.diag_ne_pins.test(from) {
            let file_diff = from.file() as i8 - to.file() as i8;
            let rank_diff = from.rank() as i8 - to.rank() as i8;
            return file_diff == rank_diff;
        }

        // 北西-南東の斜めピン
        if self.diag_nw_pins.test(from) {
            let file_diff = from.file() as i8 - to.file() as i8;
            let rank_diff = from.rank() as i8 - to.rank() as i8;
            return file_diff == -rank_diff;
        }

        false
    }
}
