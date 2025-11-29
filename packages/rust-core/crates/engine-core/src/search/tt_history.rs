//! TTMoveHistory - 置換表の指し手成功度
use super::history::StatsEntry;

/// TTMoveHistory: [ply] -> score
///
/// 置換表の指し手が最善手だった頻度を記録する。
pub struct TTMoveHistory {
    table: [StatsEntry<7183>; 256], // MAX_PLY相当
}

impl TTMoveHistory {
    pub fn new() -> Self {
        Self {
            table: [StatsEntry::default(); 256],
        }
    }

    pub fn get(&self, ply: usize) -> i16 {
        self.table[ply].get()
    }

    pub fn update(&mut self, ply: usize, bonus: i32) {
        if ply < self.table.len() {
            self.table[ply].update(bonus);
        }
    }
}

impl Default for TTMoveHistory {
    fn default() -> Self {
        Self::new()
    }
}
