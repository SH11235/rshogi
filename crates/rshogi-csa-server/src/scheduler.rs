//! Floodgate スケジュール宣言と次回起動時刻の純関数。
//!
//! 1 つの `FloodgateSchedule` は「ある `game_name` に対して、UTC の特定曜日・
//! 時刻に matchmaking を発火する」宣言。複数のスケジュールを並列に持てる。
//!
//! 本モジュールは時刻計算と TOML 入力の値型のみを提供する純粋ロジックで、
//! 実際の発火（accept ループや待機プールへのアクセス）はフロントエンド側
//! （`rshogi-csa-server-tcp/src/scheduler.rs`）が担う。
//!
//! # 設計判断
//!
//! - **UTC 固定**: 曜日 / 時刻は UTC で解釈する。日本運用では運用者が +9 時間
//!   分の換算を行う前提（DST がない / 設定が静的に追えるメリット）。
//! - **永続化なし**: スケジュールは静的な設定であり、サーバ再起動で次回時刻を
//!   再計算するのみ（YAGNI）。
//! - **タイマー抽象**: `FloodgateTimer` トレイトで「指定時刻まで待機する」操作
//!   を抽象化し、TCP（`tokio::time::sleep_until`）と Workers 将来対応
//!   （Durable Object Alarms API）を同 API で吸収する。

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc, Weekday as ChronoWeekday};
use serde::{Deserialize, Serialize};

use crate::types::GameName;

/// 曜日（Floodgate スケジュールの宣言で使う）。
///
/// `chrono::Weekday` は外部公開クレートの enum なので serde でシリアライズ
/// する際の文字列表現が安定しない可能性がある。設定ファイルで使う表現を
/// 自前で定義し、Mon/Tue/.../Sun の 3 文字頭で読み書きする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    /// `chrono::Weekday` から構築する。
    pub fn from_chrono(w: ChronoWeekday) -> Self {
        match w {
            ChronoWeekday::Mon => Self::Mon,
            ChronoWeekday::Tue => Self::Tue,
            ChronoWeekday::Wed => Self::Wed,
            ChronoWeekday::Thu => Self::Thu,
            ChronoWeekday::Fri => Self::Fri,
            ChronoWeekday::Sat => Self::Sat,
            ChronoWeekday::Sun => Self::Sun,
        }
    }

    /// `chrono::Weekday` に変換する。
    pub fn to_chrono(self) -> ChronoWeekday {
        match self {
            Self::Mon => ChronoWeekday::Mon,
            Self::Tue => ChronoWeekday::Tue,
            Self::Wed => ChronoWeekday::Wed,
            Self::Thu => ChronoWeekday::Thu,
            Self::Fri => ChronoWeekday::Fri,
            Self::Sat => ChronoWeekday::Sat,
            Self::Sun => ChronoWeekday::Sun,
        }
    }
}

/// 1 つの Floodgate スケジュール宣言。
///
/// `game_name` ごとに、UTC の `weekday` × `hour:minute` に発火する。複数の
/// スケジュールエントリを 1 game_name に紐付けたい場合は本構造体を複数並べる
/// （TOML の `[[schedules]]` 配列に複数要素を書く）形を取る。
///
/// `pairing_strategy` は文字列で受ける。`"direct"` は本タスクで配線、
/// `"least_diff"` 等は別タスクで戦略実装が入った時点で受け付ける。未知の値は
/// 起動時に `Err` で fail-fast（fronend `build_strategy_from_name` が拒否）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloodgateSchedule {
    /// CSA `LOGIN <name> <pass> <game_name>` の `game_name`。発火時に同じ
    /// game_name で待機しているプレイヤだけが対象になる。
    ///
    /// 内部表現は `String`（TOML serde 対応のため）。`GameName` への変換は
    /// [`Self::game_name`] アクセサを使う。
    pub game_name: String,
    /// 発火曜日（UTC）。
    pub weekday: Weekday,
    /// 発火時刻の時（0..=23、UTC）。
    pub hour: u8,
    /// 発火時刻の分（0..=59、UTC）。
    pub minute: u8,
    /// ペアリング戦略名。`"direct"` をフロントエンドが認識し、Floodgate 系の
    /// 追加戦略は別タスクで配線する。未知の名前は起動時 `Err`。
    pub pairing_strategy: String,
    // NOTE: per-schedule の時計（`ClockSpec`）は本タスクの範囲外。スケジュール
    // 起動の対局は `state.config.clock`（global）を使う。スケジュール毎に異なる
    // 時計を実現するには `drive_game` のシグネチャに `ClockSpec` を追加する
    // 侵襲的な改修が必要になるため、必要になった時点で別タスクで対応する。
    // 不要なフィールドを「parse はするけど無視する」silent drop は YAGNI 違反な
    // ので意図的に省く（field を持たないことで利用者の誤解を防ぐ）。
}

impl FloodgateSchedule {
    /// `game_name` を core の newtype `GameName` として取り出す。
    pub fn game_name(&self) -> GameName {
        GameName::new(&self.game_name)
    }
}

impl FloodgateSchedule {
    /// `now` 時点から見た次回発火時刻を返す。
    ///
    /// アルゴリズム:
    /// 1. 同じ曜日 + 同じ時刻の今週分を組み立てる（`now` が月曜なら、月曜の
    ///    `hour:minute`）。
    /// 2. その時刻が `now` より厳密に未来であればそれを返す（同時刻ぴったりは
    ///    「次の週」へ送り、`now == next_fire` の同一性を避ける）。
    /// 3. そうでなければ「次の同曜日」（7 日進める）を返す。
    ///
    /// 入力が常に UTC なので DST 跨ぎはなく、結果は 1 週間以内に収まる。
    pub fn next_fire_after(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let target_wd = self.weekday.to_chrono();
        let now_wd = now.weekday();

        // `weekday` の 0..=6 表現を「月曜起点」で取り、差分を mod 7 する。
        let now_idx = chrono_weekday_index(now_wd);
        let target_idx = chrono_weekday_index(target_wd);
        let mut delta_days = (target_idx + 7 - now_idx) % 7;

        // 候補日付を作る（now の日付に delta_days 加算）。
        let candidate_date = now.date_naive() + chrono::Duration::days(delta_days as i64);
        let candidate = build_utc_datetime(candidate_date, self.hour, self.minute);

        // 候補が `now` 以前なら次の週へ（厳密未来になるよう 7 日加算）。
        if candidate <= now {
            delta_days += 7;
            let next_week_date = now.date_naive() + chrono::Duration::days(delta_days as i64);
            return build_utc_datetime(next_week_date, self.hour, self.minute);
        }
        candidate
    }
}

fn chrono_weekday_index(wd: ChronoWeekday) -> u32 {
    // 月曜起点で 0..=6 を割り当てる（ISO-8601 の num_days_from_monday と同義）。
    wd.num_days_from_monday()
}

fn build_utc_datetime(date: NaiveDate, hour: u8, minute: u8) -> DateTime<Utc> {
    // `hour` / `minute` は外部入力なので 0..23 / 0..59 にクランプする。
    // 範囲外の場合は最寄りの境界値を使い、後段で fail しない（設定の早期検証は
    // TOML パース層で行われる）。
    let h = hour.min(23) as u32;
    let m = minute.min(59) as u32;
    let naive = date.and_hms_opt(h, m, 0).expect("h / m clamped to valid range");
    Utc.from_utc_datetime(&naive)
}

/// 「指定時刻まで眠る」抽象化。
///
/// 実装は frontend に持つ:
/// - `tokio::time::sleep_until` をラップする `TokioFloodgateTimer`（TCP）
/// - 将来の Durable Object Alarms API ベース実装（Workers）
pub trait FloodgateTimer {
    /// `deadline`（UTC 絶対時刻）まで待機する。`deadline <= 現在時刻` の場合は
    /// 即座に return することが望ましい（spurious tick 等の安全側挙動）。
    fn wait_until(&self, deadline: DateTime<Utc>) -> impl std::future::Future<Output = ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap()
    }

    fn schedule(weekday: Weekday, hour: u8, minute: u8) -> FloodgateSchedule {
        FloodgateSchedule {
            game_name: "floodgate-600-10".to_owned(),
            weekday,
            hour,
            minute,
            pairing_strategy: "direct".to_owned(),
        }
    }

    #[test]
    fn next_fire_returns_today_when_time_is_in_the_future() {
        // 2026-04-26 (Sun) 10:00 UTC の時点で「Sun 12:00」を狙うと、同日 12:00。
        let s = schedule(Weekday::Sun, 12, 0);
        let now = dt(2026, 4, 26, 10, 0);
        let next = s.next_fire_after(now);
        assert_eq!(next, dt(2026, 4, 26, 12, 0));
    }

    #[test]
    fn next_fire_advances_to_next_week_when_today_already_passed() {
        // 2026-04-26 (Sun) 13:00 UTC の時点で「Sun 12:00」を狙うと、来週日曜 12:00。
        let s = schedule(Weekday::Sun, 12, 0);
        let now = dt(2026, 4, 26, 13, 0);
        let next = s.next_fire_after(now);
        assert_eq!(next, dt(2026, 5, 3, 12, 0));
    }

    #[test]
    fn next_fire_advances_to_next_week_when_time_equals_now_exactly() {
        // 同時刻ぴったりは厳密未来になるよう次の週へ送る（`>` 比較のため）。
        let s = schedule(Weekday::Sun, 12, 0);
        let now = dt(2026, 4, 26, 12, 0);
        let next = s.next_fire_after(now);
        assert_eq!(next, dt(2026, 5, 3, 12, 0));
    }

    #[test]
    fn next_fire_steps_to_target_weekday_when_today_is_different() {
        // 2026-04-26 (Sun) 23:00 UTC の時点で「Wed 09:00」を狙うと、3 日後 4-29 (Wed) 09:00。
        let s = schedule(Weekday::Wed, 9, 0);
        let now = dt(2026, 4, 26, 23, 0);
        let next = s.next_fire_after(now);
        assert_eq!(next, dt(2026, 4, 29, 9, 0));
    }

    #[test]
    fn next_fire_handles_year_rollover() {
        // 大晦日 2026-12-31 (Thu) 23:30 UTC の時点で「Sat 00:00」を狙うと、
        // 2027-01-02 (Sat) 00:00 UTC。
        let s = schedule(Weekday::Sat, 0, 0);
        let now = dt(2026, 12, 31, 23, 30);
        let next = s.next_fire_after(now);
        assert_eq!(next, dt(2027, 1, 2, 0, 0));
    }

    #[test]
    fn weekday_round_trips_through_chrono() {
        for w in [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ] {
            assert_eq!(Weekday::from_chrono(w.to_chrono()), w);
        }
    }

    #[test]
    fn schedule_serde_round_trips_through_toml() {
        // TOML 設定での書式契約を固定。フィールド rename / 形式変更を CI で検知。
        let s = FloodgateSchedule {
            game_name: "floodgate-600-10".to_owned(),
            weekday: Weekday::Wed,
            hour: 9,
            minute: 30,
            pairing_strategy: "direct".to_owned(),
        };
        // GameName アクセサが正しく newtype 変換することも確認。
        assert_eq!(s.game_name().as_str(), "floodgate-600-10");
        let toml_text = toml::to_string(&s).unwrap();
        assert!(
            toml_text.contains("game_name = \"floodgate-600-10\""),
            "toml shape: {toml_text}",
        );
        assert!(toml_text.contains("weekday = \"Wed\""), "toml shape: {toml_text}");
        assert!(toml_text.contains("pairing_strategy = \"direct\""));
        let parsed: FloodgateSchedule = toml::from_str(&toml_text).unwrap();
        assert_eq!(parsed, s);
    }

    /// 月末跨ぎ: 2026-04-30 (Thu) 23:30 UTC の時点で「Fri 00:00」を狙うと、
    /// 2026-05-01 (Fri) 00:00 UTC（日数 +1、月またぎ）。
    #[test]
    fn next_fire_handles_month_rollover() {
        let s = schedule(Weekday::Fri, 0, 0);
        let now = dt(2026, 4, 30, 23, 30);
        let next = s.next_fire_after(now);
        assert_eq!(next, dt(2026, 5, 1, 0, 0));
    }

    /// 閏年跨ぎ: 2024-02-28 (Wed) 12:00 UTC の時点で「Sat 00:00」を狙うと、
    /// 閏日 2024-02-29 (Thu) を経て 2024-03-02 (Sat) 00:00 UTC。
    #[test]
    fn next_fire_handles_leap_day_rollover() {
        let s = schedule(Weekday::Sat, 0, 0);
        let now = dt(2024, 2, 28, 12, 0);
        let next = s.next_fire_after(now);
        assert_eq!(next, dt(2024, 3, 2, 0, 0));
    }

    /// 閏年の 2-29 (Thu) 自身を起点にしたケース。「Thu 23:00」を狙うと同日 23:00。
    #[test]
    fn next_fire_handles_leap_day_as_origin() {
        let s = schedule(Weekday::Thu, 23, 0);
        let now = dt(2024, 2, 29, 12, 0);
        let next = s.next_fire_after(now);
        assert_eq!(next, dt(2024, 2, 29, 23, 0));
    }

    #[test]
    fn build_utc_datetime_clamps_out_of_range_inputs_to_boundary() {
        // 範囲外入力（hour=99, minute=99）は 23:59 にクランプする防衛挙動。
        // 通常は TOML パース層で fail するが、build_utc_datetime 単体で安全側に
        // 落ちることを契約として固定する。
        let date = NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let dt_clamped = build_utc_datetime(date, 99, 99);
        assert_eq!(dt_clamped, dt(2026, 4, 26, 23, 59));
    }
}
