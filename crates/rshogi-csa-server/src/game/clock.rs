//! 持ち時間管理 — 3 方式のうち Phase 1 では秒読み方式のみ実装する。
//!
//! 秒読み方式（[`SecondsCountdownClock`]）は CSA 2014 改訂互換で、
//! `Least_Time_Per_Move = 0`、経過時間は整数秒に切り捨てる。
//!
//! # API 設計メモ
//!
//! 残時間系 API は 2 種類に分かれる。意味を取り違えると deadline 計算を誤るため、
//! 呼び出し側は用途に応じて使い分けること。
//!
//! - [`TimeClock::remaining_main_ms`][]: **表示・ロギング用**の本体時間残り。
//!   秒読みは含まない。対局者向け Game_Summary や GUI 表示で使う。
//! - [`TimeClock::turn_budget_ms`][]: **deadline 計算用**の「今の 1 手で使える最大時間」。
//!   秒読みは手番ごとにリセットされるため、`本体残り + byoyomi` 全量 を返す。
//!   `run_loop::compute_deadline` などの時間切れアラームはこちらを使う。
//!
//! 旧 `remaining_ms` は意味がぶれて deadline 計算側で秒読みを無視するバグを招いたため、
//! 本 Phase 1 で API を明示的に分離する破壊的変更を入れている。

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
    /// 呼び出し側が通信マージンを差し引いて渡すこと。
    fn consume(&mut self, color: Color, elapsed_ms: u64) -> ClockResult;

    /// Game_Summary の `BEGIN Time` セクションを CSA 仕様の項目・順序・単位で出力する。
    fn format_summary(&self) -> String;

    /// 指定対局者の **本体持ち時間** の残り（ミリ秒）。
    ///
    /// 秒読みは含めない。GUI 表示・ログ・`HandleOutcome::MoveAccepted` の通知など、
    /// 人間向けの情報提示で使う。0 を下回らずクランプされていてよい。
    /// 型が `i64` なのは将来他方式の時計で負値を許容する余地を残すため。
    fn remaining_main_ms(&self, color: Color) -> i64;

    /// 指定対局者が **今の 1 手で使える最大時間** をミリ秒で返す。
    ///
    /// `run_loop::compute_deadline` など時間切れアラームの算出に使う。
    /// 秒読み方式では `本体残り + byoyomi` を返す（秒読みは手番開始でリセットされるため
    /// 前手の消費は引かない）。Fischer / StopWatch 方式も同じ意味で実装する。
    fn turn_budget_ms(&self, color: Color) -> i64;
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

    fn slot(&self, color: Color) -> i64 {
        match color {
            Color::Black => self.remaining_black_ms,
            Color::White => self.remaining_white_ms,
        }
    }

    /// `byoyomi_seconds` をミリ秒単位で返す（ヘルパ）。
    fn byoyomi_ms(&self) -> i64 {
        self.byoyomi_seconds as i64 * 1000
    }
}

impl TimeClock for SecondsCountdownClock {
    fn consume(&mut self, color: Color, elapsed_ms: u64) -> ClockResult {
        // 整数秒に切り捨て（CSA 2014 改訂）。
        let elapsed_sec = (elapsed_ms / 1000) as i64;
        let byoyomi_ms = self.byoyomi_ms();
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

    fn remaining_main_ms(&self, color: Color) -> i64 {
        // 本体時間のみ。秒読みは手番ごとにリセットされるので残量の概念は無い。
        self.slot(color)
    }

    fn turn_budget_ms(&self, color: Color) -> i64 {
        // 今の 1 手で使える最大時間 = 本体残り + 毎手 full 回復する byoyomi。
        self.slot(color) + self.byoyomi_ms()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continues_when_consume_within_main_time() {
        let mut c = SecondsCountdownClock::new(600, 10);
        assert_eq!(c.consume(Color::Black, 1_200), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 599_000);
    }

    #[test]
    fn truncates_sub_second() {
        let mut c = SecondsCountdownClock::new(10, 0);
        // 999ms は 0 秒に切り捨てられる
        assert_eq!(c.consume(Color::Black, 999), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 10_000);
    }

    #[test]
    fn enters_byoyomi_when_main_exhausted() {
        let mut c = SecondsCountdownClock::new(5, 10);
        // 本体 5 秒ちょうど消費で、本体は 0、秒読みに残り 10 秒相当
        assert_eq!(c.consume(Color::Black, 5_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 0);
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
        assert_eq!(c.remaining_main_ms(Color::White), 60_000);
    }

    // ---- 秒読み / turn_budget_ms 回帰テスト ----

    #[test]
    fn turn_budget_includes_byoyomi_on_fresh_clock() {
        // 本体 60 秒 + 秒読み 10 秒 → 初手の予算 70 秒。旧 API (remaining_ms) は 60 秒しか
        // 返さず、deadline 計算が byoyomi を無視するバグの元だった。
        let c = SecondsCountdownClock::new(60, 10);
        assert_eq!(c.remaining_main_ms(Color::Black), 60_000);
        assert_eq!(c.turn_budget_ms(Color::Black), 70_000);
    }

    #[test]
    fn turn_budget_is_byoyomi_only_after_main_exhausted() {
        // 本体 5 秒使い切り後、各手番は byoyomi 10 秒だけが予算。
        let mut c = SecondsCountdownClock::new(5, 10);
        assert_eq!(c.consume(Color::Black, 5_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 0);
        assert_eq!(c.turn_budget_ms(Color::Black), 10_000);
        // 次の手番も同じ予算（byoyomi はリセットされる）。
        assert_eq!(c.consume(Color::Black, 9_000), ClockResult::Continue);
        assert_eq!(c.turn_budget_ms(Color::Black), 10_000);
    }

    #[test]
    fn turn_budget_zero_only_when_main_zero_and_byoyomi_zero() {
        // byoyomi=0 かつ本体 0 でだけ予算 0（= 次の手で即 time-up）。
        let mut c = SecondsCountdownClock::new(5, 0);
        assert_eq!(c.consume(Color::Black, 5_000), ClockResult::Continue);
        assert_eq!(c.turn_budget_ms(Color::Black), 0);
    }
}
