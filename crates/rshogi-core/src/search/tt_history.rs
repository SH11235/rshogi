//! TTMoveHistory - 置換表の指し手成功度
//!
//! YaneuraOu準拠: ワーカーレベルの単一値（i16, cap 8192）
//! - Multi-cut 時: `<< max(-400 - 100*depth, -4000)`
//! - search終了時: `<< (bestMove == ttMove ? 809 : -865)`
//! - Step 15 doubleMargin で使用: `- 921 * ttMoveHistory / 127649`

use super::history::StatsEntry;

/// TTMoveHistory 更新定数（YaneuraOu準拠）
pub const TT_MOVE_HISTORY_MULTI_CUT_BASE: i32 = -400;
pub const TT_MOVE_HISTORY_MULTI_CUT_DEPTH_MULT: i32 = -100;
pub const TT_MOVE_HISTORY_MULTI_CUT_MIN: i32 = -4000;

/// TTMoveHistory: ワーカーレベルの単一値
///
/// 置換表の指し手が最善手だった頻度を記録する。
/// YaneuraOu準拠: ply ではなく、ワーカー全体で単一の値を管理。
pub struct TTMoveHistory {
    entry: StatsEntry<8192>,
}

impl TTMoveHistory {
    pub fn new() -> Self {
        Self {
            entry: StatsEntry::default(),
        }
    }

    /// 値を取得
    #[inline]
    pub fn get(&self) -> i16 {
        self.entry.get()
    }

    /// ボーナス値で更新
    #[inline]
    pub fn update(&mut self, bonus: i32) {
        self.entry.update(bonus);
    }

    /// Multi-cut時の更新ボーナスを計算
    ///
    /// YaneuraOu準拠: `max(-400 - 100 * depth, -4000)`
    #[inline]
    pub fn multi_cut_bonus(depth: i32) -> i32 {
        (TT_MOVE_HISTORY_MULTI_CUT_BASE + TT_MOVE_HISTORY_MULTI_CUT_DEPTH_MULT * depth)
            .max(TT_MOVE_HISTORY_MULTI_CUT_MIN)
    }

    /// クリア
    pub fn clear(&mut self) {
        self.entry.set(0);
    }
}

impl Default for TTMoveHistory {
    fn default() -> Self {
        Self::new()
    }
}
