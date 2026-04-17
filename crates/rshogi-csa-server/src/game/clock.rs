//! 持ち時間管理 — 3 方式のうち Phase 1 では秒読み方式のみ実装する。
//!
//! 秒読み方式（[`SecondsCountdownClock`]）は CSA 2014 改訂互換で、
//! `Least_Time_Per_Move = 0`、経過時間は整数秒に切り捨てる。

use crate::types::Color;

/// 1 手消費後の時計判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockResult {
    /// 対局続行可能。
    Continue,
    /// 時間切れ。手番プレイヤ敗北。
    TimeUp,
}

/// 持ち時間管理の抽象。3 方式（秒読み/Fischer/StopWatch）の共通インタフェース。
pub trait TimeClock {
    /// 指定した対局者の残時間から `elapsed_ms` ミリ秒分を消費し、時間切れ判定を返す。
    ///
    /// 呼び出し側が通信マージンを差し引いて渡すこと（Requirement 3.6）。
    fn consume(&mut self, color: Color, elapsed_ms: u64) -> ClockResult;

    /// Game_Summary の `BEGIN Time` セクションを CSA 仕様の項目・順序・単位で出力する。
    fn format_summary(&self) -> String;

    /// 指定対局者の残時間（ミリ秒）。
    ///
    /// 本体時間と秒読みを合算した残時間を返す。
    /// 実装は 0 を下回らない値にクランプして返してよい
    /// （[`SecondsCountdownClock`] は 0 止まり）。
    /// 型が `i64` なのは将来他方式の時計で負値を許容する余地を残すため。
    fn remaining_ms(&self, color: Color) -> i64;
}

/// 秒読み方式の時計（CSA 2014 改訂互換）。
///
/// - `total_time_seconds`: 持ち時間本体（秒）。使い切ると秒読みへ移行。
/// - `byoyomi_seconds`: 1 手あたりの秒読み時間（秒）。使い切ると時間切れ。
/// - `least_time_per_move`: CSA 2014 改訂では `0`。本実装も `0` 固定。
/// - 経過時間は整数秒に切り捨て（`elapsed_sec = elapsed_ms / 1000`）。
#[derive(Debug, Clone)]
pub struct SecondsCountdownClock {
    total_time_seconds: u32,
    byoyomi_seconds: u32,
    remaining_black_ms: i64,
    remaining_white_ms: i64,
}

impl SecondsCountdownClock {
    /// 新しい秒読み時計を作る。
    ///
    /// 引数の単位はいずれも「秒」。内部は負値許容のミリ秒で保持する。
    pub fn new(total_time_seconds: u32, byoyomi_seconds: u32) -> Self {
        let initial = total_time_seconds as i64 * 1000;
        Self {
            total_time_seconds,
            byoyomi_seconds,
            remaining_black_ms: initial,
            remaining_white_ms: initial,
        }
    }

    fn slot_mut(&mut self, color: Color) -> &mut i64 {
        match color {
            Color::Black => &mut self.remaining_black_ms,
            Color::White => &mut self.remaining_white_ms,
        }
    }
}

impl TimeClock for SecondsCountdownClock {
    fn consume(&mut self, color: Color, elapsed_ms: u64) -> ClockResult {
        // 整数秒に切り捨て（CSA 2014 改訂）。
        let elapsed_sec = (elapsed_ms / 1000) as i64;
        let byoyomi_ms = self.byoyomi_seconds as i64 * 1000;
        let slot = self.slot_mut(color);

        // 本体持ち時間の中で収まる場合は単純に減算する。
        if elapsed_sec * 1000 <= *slot {
            *slot -= elapsed_sec * 1000;
            return ClockResult::Continue;
        }

        // 本体を超過した場合は、本体分だけ 0 に落として秒読みに乗り換える。
        let over_sec = elapsed_sec - (*slot / 1000);
        *slot = 0;
        if over_sec * 1000 > byoyomi_ms {
            // 秒読みを使い切った
            ClockResult::TimeUp
        } else {
            ClockResult::Continue
        }
    }

    fn format_summary(&self) -> String {
        // CSA 仕様の `BEGIN Time` セクション項目順:
        //   Time_Unit, Total_Time, Byoyomi, Least_Time_Per_Move
        let mut out = String::new();
        out.push_str("BEGIN Time\n");
        out.push_str("Time_Unit:1sec\n");
        out.push_str(&format!("Total_Time:{}\n", self.total_time_seconds));
        out.push_str(&format!("Byoyomi:{}\n", self.byoyomi_seconds));
        out.push_str("Least_Time_Per_Move:0\n");
        out.push_str("END Time\n");
        out
    }

    fn remaining_ms(&self, color: Color) -> i64 {
        match color {
            Color::Black => self.remaining_black_ms,
            Color::White => self.remaining_white_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continues_when_consume_within_main_time() {
        let mut c = SecondsCountdownClock::new(600, 10);
        assert_eq!(c.consume(Color::Black, 1_200), ClockResult::Continue);
        assert_eq!(c.remaining_ms(Color::Black), 599_000);
    }

    #[test]
    fn truncates_sub_second() {
        let mut c = SecondsCountdownClock::new(10, 0);
        // 999ms は 0 秒に切り捨てられる
        assert_eq!(c.consume(Color::Black, 999), ClockResult::Continue);
        assert_eq!(c.remaining_ms(Color::Black), 10_000);
    }

    #[test]
    fn enters_byoyomi_when_main_exhausted() {
        let mut c = SecondsCountdownClock::new(5, 10);
        // 本体 5 秒ちょうど消費で、本体は 0、秒読みに残り 10 秒相当
        assert_eq!(c.consume(Color::Black, 5_000), ClockResult::Continue);
        assert_eq!(c.remaining_ms(Color::Black), 0);
        // 以降、秒読み 10 秒以内であれば TimeUp にならない
        assert_eq!(c.consume(Color::Black, 9_000), ClockResult::Continue);
    }

    #[test]
    fn time_up_when_over_byoyomi() {
        let mut c = SecondsCountdownClock::new(5, 10);
        // 本体 5 秒 + 秒読み 11 秒 = 16 秒 消費
        assert_eq!(c.consume(Color::Black, 16_000), ClockResult::TimeUp);
    }

    #[test]
    fn format_summary_contains_csa_fields() {
        let c = SecondsCountdownClock::new(600, 10);
        let s = c.format_summary();
        assert!(s.contains("BEGIN Time"));
        assert!(s.contains("Time_Unit:1sec"));
        assert!(s.contains("Total_Time:600"));
        assert!(s.contains("Byoyomi:10"));
        assert!(s.contains("Least_Time_Per_Move:0"));
        assert!(s.contains("END Time"));
    }

    #[test]
    fn black_and_white_are_independent() {
        let mut c = SecondsCountdownClock::new(60, 5);
        assert_eq!(c.consume(Color::Black, 10_000), ClockResult::Continue);
        // 白の残時間は減らない
        assert_eq!(c.remaining_ms(Color::White), 60_000);
    }
}
