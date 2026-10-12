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
    ///
    /// byoyomi 時: 秒読み + 残り時間の按分を上限とする。
    /// フィッシャー時: 残り時間 + 加算をそのまま上限とする。
    /// エンジンは `go btime/wtime/binc/winc` で時間情報を受け取り自前で配分するため、
    /// ツール側は残り時間全体をタイムアウト上限として信頼する。
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
        // フィッシャー: エンジンの時間管理を信頼し、残り時間 + 加算を上限とする。
        // エンジンは重要局面で残り時間の大部分を1手に使うことがあるため、
        // per-move budget で制限するとタイムアウト誤判定が発生する。
        remaining.saturating_add(inc).max(MIN_THINK_MS)
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

    /// 1手指した後の残り時間を返す。
    ///
    /// フィッシャー時は今回の手に使える時間が「残り時間 + 加算」（`think_limit_ms` 参照）なので、
    /// 加算を足してから消費時間を引く。先に消費時間を引くと、残り時間を超えて加算分へ
    /// 食い込んだ消費が 0 で打ち切られ、残り時間が加算の値まで戻ってしまう。
    pub fn updated_time(&self, current: u64, inc: u64, elapsed_ms: u64) -> u64 {
        if self.byoyomi > 0 {
            let over = elapsed_ms.saturating_sub(self.byoyomi);
            return current.saturating_sub(over).saturating_add(inc);
        }
        current.saturating_add(inc).saturating_sub(elapsed_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_control_allocates_fractional_budget() {
        // byoyomi: per-move budget + byoyomi
        let tc = TimeControl::new(60_000, 60_000, 0, 0, 1_000);
        assert_eq!(tc.think_limit_ms(Color::Black), 2_500);
        assert_eq!(tc.updated_time(60_000, 0, 1_500), 59_500);

        // フィッシャー: 残り時間 + 加算をそのまま返す
        let tc_inc = TimeControl::new(60_000, 60_000, 1_000, 0, 0);
        assert_eq!(tc_inc.think_limit_ms(Color::Black), 61_000);
        assert_eq!(tc_inc.updated_time(5_000, 1_000, 4_000), 2_000);
    }

    #[test]
    fn fischer_clock_charges_time_spent_from_increment() {
        let mut tc = TimeControl::new(500, 500, 1_000, 1_000, 0);
        // 残り 500ms + 加算 1000ms のうち 1400ms を使えば、残るのは 100ms。
        assert_eq!(tc.think_limit_ms(Color::Black), 1_500);
        tc.update_after_move(Color::Black, 1_400);
        assert_eq!(tc.remaining(Color::Black), 100);
        assert_eq!(tc.remaining(Color::White), 500);
        // 次の手は 100ms + 1000ms が上限で、使い切れば 0 になる。
        assert_eq!(tc.think_limit_ms(Color::Black), 1_100);
        tc.update_after_move(Color::Black, 1_100);
        assert_eq!(tc.remaining(Color::Black), 0);
        // 判定マージン内の超過は時間切れにならないが、時計は 0 で止まり加算へは戻らない。
        tc.update_after_move(Color::Black, 1_050);
        assert_eq!(tc.remaining(Color::Black), 0);
    }
}
