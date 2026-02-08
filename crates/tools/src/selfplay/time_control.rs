use rshogi_core::types::Color;

use super::types::TimeArgs;

/// 残り約40手を想定して1手あたりの持ち時間を配分する。
const TIME_ALLOCATION_MOVES: u64 = 40;
const MIN_THINK_MS: u64 = 10;

/// USI 互換の時間管理を最低限行うヘルパー。
#[derive(Clone, Copy)]
pub struct TimeControl {
    pub black_time: u64,
    pub white_time: u64,
    pub black_inc: u64,
    pub white_inc: u64,
    pub byoyomi: u64,
}

impl TimeControl {
    pub fn new(btime: u64, wtime: u64, binc: u64, winc: u64, byoyomi: u64) -> Self {
        Self {
            black_time: btime,
            white_time: wtime,
            black_inc: binc,
            white_inc: winc,
            byoyomi,
        }
    }

    pub fn time_args(&self) -> TimeArgs {
        TimeArgs {
            btime: self.black_time,
            wtime: self.white_time,
            byoyomi: self.byoyomi,
            binc: self.black_inc,
            winc: self.white_inc,
        }
    }

    /// 残り時間を分割して1手あたりの思考上限を決める。
    pub fn think_limit_ms(&self, side: Color) -> u64 {
        let remaining = self.remaining(side);
        let inc = self.increment_for(side);
        if self.byoyomi > 0 {
            let available = remaining.saturating_add(self.byoyomi);
            let per_move_budget = remaining / TIME_ALLOCATION_MOVES;
            let candidate = self.byoyomi.saturating_add(per_move_budget);
            let lower = self.byoyomi.max(MIN_THINK_MS.min(available));
            return candidate.clamp(lower, available);
        }
        let per_move_budget = remaining / TIME_ALLOCATION_MOVES;
        let candidate = per_move_budget.saturating_add(inc);
        let lower = MIN_THINK_MS.min(remaining);
        candidate.clamp(lower, remaining)
    }

    pub fn remaining(&self, side: Color) -> u64 {
        if side == Color::Black {
            self.black_time
        } else {
            self.white_time
        }
    }

    pub fn increment_for(&self, side: Color) -> u64 {
        if side == Color::Black {
            self.black_inc
        } else {
            self.white_inc
        }
    }

    pub fn update_after_move(&mut self, side: Color, elapsed_ms: u64) {
        if side == Color::Black {
            self.black_time = self.updated_time(self.black_time, self.black_inc, elapsed_ms);
        } else {
            self.white_time = self.updated_time(self.white_time, self.white_inc, elapsed_ms);
        }
    }

    pub fn updated_time(&self, current: u64, inc: u64, elapsed_ms: u64) -> u64 {
        let mut next = current;
        if self.byoyomi > 0 {
            let over = elapsed_ms.saturating_sub(self.byoyomi);
            next = next.saturating_sub(over);
        } else {
            next = next.saturating_sub(elapsed_ms);
        }
        next = next.saturating_add(inc);
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_control_allocates_fractional_budget() {
        let tc = TimeControl::new(60_000, 60_000, 0, 0, 1_000);
        assert_eq!(tc.think_limit_ms(Color::Black), 2_500);
        assert_eq!(tc.updated_time(60_000, 0, 1_500), 59_500);

        let tc_inc = TimeControl::new(60_000, 60_000, 1_000, 0, 0);
        assert_eq!(tc_inc.think_limit_ms(Color::Black), 2_500);
        assert_eq!(tc_inc.updated_time(5_000, 1_000, 4_000), 2_000);
    }
}
