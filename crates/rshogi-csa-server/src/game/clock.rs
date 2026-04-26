//! 持ち時間管理。現状は秒読み方式 [`SecondsCountdownClock`] のみ実装する。
//!
//! 秒読み方式は CSA 2014 改訂互換で、`Least_Time_Per_Move = 0`、経過時間は
//! 整数秒に切り捨てる。
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
//! 意味が曖昧な単一の `remaining_ms` にせず明確に 2 種類へ分けているのは、
//! deadline 計算側で秒読みを無視するバグを防ぐため。

use serde::{Deserialize, Serialize};

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

/// フロントエンド設定から選択する時計方式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClockSpec {
    /// CSA 2014 改訂互換の秒読み。
    Countdown {
        total_time_sec: u32,
        byoyomi_sec: u32,
    },
    /// Fischer 方式（秒単位）。
    Fischer {
        total_time_sec: u32,
        increment_sec: u32,
    },
    /// StopWatch 方式（分単位）。
    StopWatch {
        total_time_min: u32,
        byoyomi_min: u32,
    },
}

impl Default for ClockSpec {
    fn default() -> Self {
        Self::Countdown {
            total_time_sec: 600,
            byoyomi_sec: 10,
        }
    }
}

impl ClockSpec {
    /// 指定設定に対応する時計インスタンスを生成する。
    pub fn build_clock(&self) -> Box<dyn TimeClock> {
        match self {
            Self::Countdown {
                total_time_sec,
                byoyomi_sec,
            } => Box::new(SecondsCountdownClock::new(*total_time_sec, *byoyomi_sec)),
            Self::Fischer {
                total_time_sec,
                increment_sec,
            } => Box::new(FischerClock::new(*total_time_sec, *increment_sec)),
            Self::StopWatch {
                total_time_min,
                byoyomi_min,
            } => Box::new(StopWatchClock::new(*total_time_min, *byoyomi_min)),
        }
    }

    /// `Game_Summary` / 棋譜へ埋め込む時間セクションを生成する。
    pub fn format_time_section(&self) -> String {
        self.build_clock().format_summary()
    }
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
        //
        // `consume` は経過時間を **秒単位に切り捨て** てから差し引くため、
        // 実際に受理される物理 elapsed_ms は `slot + byoyomi_ms` そのものではなく
        // 「次の秒境界の直前まで」 (`slot + byoyomi + 999ms`) となる。スケジューラは
        // 本関数の戻り値で deadline を設定するため、`consume` の truncation 分
        // (999ms) を足さないと正当な着手がタイムアウトで強制敗北する。
        self.slot(color) + self.byoyomi_ms() + (SECOND_GRAIN_MS - 1)
    }
}

/// 1 秒分のミリ秒。Fischer / SecondsCountdown の grain (最小単位) として共通使用。
const SECOND_GRAIN_MS: i64 = 1_000;
/// 1 分分のミリ秒。StopWatch の grain (最小単位)。
const MINUTE_GRAIN_MS: i64 = 60 * 1_000;

/// Fischer 方式の時計（増分加算、**CSA client の会計規則に合わせた init+post hybrid**）。
///
/// - `total_time_seconds`: 初期の持ち時間（秒）。
/// - `increment_seconds`: 1 手ごとに加算される増分（秒）。
/// - 経過時間は整数秒に切り捨て（SecondsCountdown と同様 CSA 慣用）。
/// - 消費で残時間が負に落ちた時点で時間切れ。
///
/// # セマンティクス
/// 既存 CSA client (`crates/tools/src/csa_client/session.rs`) は以下の会計を採用:
/// ```text
/// init:          slot = total + increment     // pre-init-increment
/// consume(e):    slot = slot - e + increment  // post-move-increment
/// ```
///
/// FIDE 標準 (init=total / 各手 post-increment) とも、完全な pre-increment
/// (init=total / 各手 pre-increment) とも異なる独特の計算だが、既存 client /
/// 棋譜ツールとの interop を優先してサーバもこれに合わせる。初手に
/// `total + increment` 秒の予算があり、以後各手ごとに (残 - elapsed + inc) で
/// slot が更新される。
///
/// - `turn_budget_ms` は「現在の slot (既に +increment 済み) + 秒 grain」を返す。
#[derive(Debug, Clone)]
pub struct FischerClock {
    total_time_seconds: u32,
    increment_seconds: u32,
    remaining_black_ms: i64,
    remaining_white_ms: i64,
}

impl FischerClock {
    /// 新しい Fischer 時計を作る。引数単位は「秒」。
    ///
    /// client と合わせるため初期 slot は `total + increment` (= 初手で使える
    /// 予算)。以後 `consume` 毎に post-increment で `slot = slot - elapsed + inc`。
    pub fn new(total_time_seconds: u32, increment_seconds: u32) -> Self {
        let initial = (total_time_seconds as i64 + increment_seconds as i64) * 1000;
        Self {
            total_time_seconds,
            increment_seconds,
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

    fn increment_ms(&self) -> i64 {
        self.increment_seconds as i64 * 1000
    }
}

impl TimeClock for FischerClock {
    fn consume(&mut self, color: Color, elapsed_ms: u64) -> ClockResult {
        // CSA client の会計規則に合わせた post-move-increment:
        //   new_slot = slot - elapsed + increment
        // 初期 slot は `new` で `total + increment` 済みなので、初手でも
        // `total + increment` 秒の予算を検査できる。2 手目以降は前手の完了時に
        // increment が加算されているため実質的に post-increment 挙動。
        // client は `black_time_ms = total + inc` + 毎手 `slot -= e; slot += inc`
        // で動くので、server もこれに一致させる。
        let elapsed_sec_ms = (elapsed_ms / 1000) as i64 * 1000;
        let increment = self.increment_ms();
        let slot = self.slot_mut(color);

        let after_consume = *slot - elapsed_sec_ms;
        if after_consume < 0 {
            *slot = 0;
            ClockResult::TimeUp
        } else {
            *slot = after_consume + increment;
            ClockResult::Continue
        }
    }

    fn format_summary(&self) -> String {
        // Fischer 方式では `Byoyomi` の代わりに `Increment` を出力する。
        // 項目順は CSA 仕様互換の `Time_Unit, Total_Time, Increment, Least_Time_Per_Move`。
        let mut out = String::new();
        out.push_str("BEGIN Time\n");
        out.push_str("Time_Unit:1sec\n");
        out.push_str(&format!("Total_Time:{}\n", self.total_time_seconds));
        out.push_str(&format!("Increment:{}\n", self.increment_seconds));
        out.push_str("Least_Time_Per_Move:0\n");
        out.push_str("END Time\n");
        out
    }

    fn remaining_main_ms(&self, color: Color) -> i64 {
        self.slot(color)
    }

    fn turn_budget_ms(&self, color: Color) -> i64 {
        // `slot` は既に前手 post-increment 込み (初期は `total + increment`、
        // 以後 `- elapsed + increment`)。`consume` は `slot - elapsed` で判定
        // するので、deadline も現在の slot 値をそのまま budget とする。
        // `consume` の秒切り捨てを考慮して `SECOND_GRAIN_MS - 1` (999ms) を足す。
        self.slot(color) + (SECOND_GRAIN_MS - 1)
    }
}

/// StopWatch 方式の時計（分単位切り捨ての秒読み）。
///
/// CSA 2014 改訂以前の shogi-server 標準挙動に相当する。
/// - 持ち時間・秒読みとも **分単位** で扱う (`Time_Unit:1min`)。
/// - 経過時間は **分単位に切り捨て** される。具体的には `elapsed_sec / 60` を
///   消費分として差し引く。これにより 0〜59 秒の手は時間消費ゼロ、60 秒以上で
///   初めて 1 分消費される。
/// - 本体持ち時間を使い切った後は、毎手分の秒読み（= `byoyomi_minutes` 分）に
///   乗り換える。秒読み中も 1 手で使える時間は `byoyomi_minutes` 分に固定。
/// - 秒読みを使い切ったら時間切れ。
#[derive(Debug, Clone)]
pub struct StopWatchClock {
    total_time_minutes: u32,
    byoyomi_minutes: u32,
    remaining_black_ms: i64,
    remaining_white_ms: i64,
}

impl StopWatchClock {
    /// 新しい StopWatch 時計を作る。引数単位は「分」。
    pub fn new(total_time_minutes: u32, byoyomi_minutes: u32) -> Self {
        let initial = total_time_minutes as i64 * 60 * 1000;
        Self {
            total_time_minutes,
            byoyomi_minutes,
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

    /// `byoyomi_minutes` をミリ秒単位で返す（ヘルパ）。
    fn byoyomi_ms(&self) -> i64 {
        self.byoyomi_minutes as i64 * 60 * 1000
    }
}

impl TimeClock for StopWatchClock {
    fn consume(&mut self, color: Color, elapsed_ms: u64) -> ClockResult {
        // 分単位切り捨て。elapsed_min_ms = floor(elapsed_ms / 60000) * 60000。
        let elapsed_min = elapsed_ms / 60_000;
        let elapsed_min_ms = (elapsed_min as i64) * 60 * 1000;
        let byoyomi_ms = self.byoyomi_ms();
        let slot = self.slot_mut(color);

        // 本体時間に収まる場合は単純減算。
        if elapsed_min_ms <= *slot {
            *slot -= elapsed_min_ms;
            return ClockResult::Continue;
        }

        // 本体を超過した分は秒読みに回す。SecondsCountdown と同じロジックを
        // 分単位で再実装しているため一見冗長に見えるが、単位を混ぜて bug を
        // 招かないよう別構造体として明示的に保持している。
        let over_min_ms = elapsed_min_ms - *slot;
        *slot = 0;
        if over_min_ms > byoyomi_ms {
            ClockResult::TimeUp
        } else {
            ClockResult::Continue
        }
    }

    fn format_summary(&self) -> String {
        // StopWatch 方式は `consume` が elapsed_ms を **分単位に切り捨てる** ため、
        // Game_Summary も CSA 仕様の `Time_Unit:1min` で分単位を宣言する。
        //
        // # 既知の client-server 乖離
        //
        // 本 server は `T<sec>` broadcast (秒単位の elapsed) を送り、client
        // (`crates/tools/src/csa_client/session.rs`) はその値を literal ms として
        // 残時間から減算する。一方 server 側の `consume` は分単位で切り捨てる
        // ため、「client のローカル remaining」と「server の実 slot」は 1 手に
        // 最大 59 秒ずれ得る（client 側実装の limitation）。engine の時間管理
        // 精度を保つには、client 側も StopWatch 相当の分単位切り捨てを行うか、
        // T<sec> を分単位で emit する必要がある。
        //
        // 現状の妥協: server は CSA 仕様に準拠して `Time_Unit:1min` を出し、
        // client-side の取り違えは後続タスクで修正する (サーバ側を変えると
        // `%%LIST` / 棋譜互換を広く破ってしまう)。
        let mut out = String::new();
        out.push_str("BEGIN Time\n");
        out.push_str("Time_Unit:1min\n");
        out.push_str(&format!("Total_Time:{}\n", self.total_time_minutes));
        out.push_str(&format!("Byoyomi:{}\n", self.byoyomi_minutes));
        out.push_str("Least_Time_Per_Move:0\n");
        out.push_str("END Time\n");
        out
    }

    fn remaining_main_ms(&self, color: Color) -> i64 {
        self.slot(color)
    }

    fn turn_budget_ms(&self, color: Color) -> i64 {
        // 本体残り + 毎手 full 回復する秒読み（分単位）。
        //
        // `consume` は elapsed_ms を **分単位に切り捨て** (`elapsed_ms / 60_000`)
        // してから差し引くため、`slot + byoyomi_ms` で deadline を切るとプレイヤが
        // まだ本来使える分 (最大 59_999ms) を失う。`MINUTE_GRAIN_MS - 1` を足して
        // 次の分境界の直前まで deadline を伸ばし、`consume` の切り捨て挙動と整合させる。
        self.slot(color) + self.byoyomi_ms() + (MINUTE_GRAIN_MS - 1)
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
        // 本体 60 秒 + 秒読み 10 秒 → 初手の予算 70_999ms (秒 grain の `consume` で
        // truncation される最大フラクショナル 999ms を scheduler deadline に含める)。
        // 旧 API (remaining_ms) は 60 秒しか返さず、deadline 計算が byoyomi を無視する
        // バグの元だった。
        let c = SecondsCountdownClock::new(60, 10);
        assert_eq!(c.remaining_main_ms(Color::Black), 60_000);
        assert_eq!(c.turn_budget_ms(Color::Black), 70_999);
    }

    #[test]
    fn turn_budget_is_byoyomi_only_after_main_exhausted() {
        // 本体 5 秒使い切り後、各手番は byoyomi 10 秒 + 秒 grain 分 = 10_999ms が予算。
        let mut c = SecondsCountdownClock::new(5, 10);
        assert_eq!(c.consume(Color::Black, 5_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 0);
        assert_eq!(c.turn_budget_ms(Color::Black), 10_999);
        // 次の手番も同じ予算（byoyomi はリセットされる）。
        assert_eq!(c.consume(Color::Black, 9_000), ClockResult::Continue);
        assert_eq!(c.turn_budget_ms(Color::Black), 10_999);
    }

    #[test]
    fn turn_budget_reflects_second_grain_when_byoyomi_zero() {
        // byoyomi=0 で本体 0 に落ちた状態でも、秒 grain の切り捨て分 (999ms) だけ
        // deadline を伸ばす必要がある。`consume(elapsed=999)` は elapsed_sec=0 に
        // 切り捨てられるため受理される (本体 0 を消費しない)。budget もこれに
        // 合わせ 999ms を返すのが正しい挙動。
        let mut c = SecondsCountdownClock::new(5, 0);
        assert_eq!(c.consume(Color::Black, 5_000), ClockResult::Continue);
        assert_eq!(c.turn_budget_ms(Color::Black), 999);
    }

    // ---- FischerClock ----

    #[test]
    fn fischer_matches_csa_client_accounting() {
        // CSA client は init `slot = total + inc` + 毎手 `slot -= e; slot += inc`
        // で動くので、server もこれに一致させる。
        //
        // 初期 60 秒、増分 5 秒。init で slot=65。move 1 で 10 秒使うと:
        //   slot = 65 - 10 = 55, + inc = 60
        // move 2 で 2 秒使うと:
        //   slot = 60 - 2 = 58, + inc = 63
        let mut c = FischerClock::new(60, 5);
        assert_eq!(c.remaining_main_ms(Color::Black), 65_000);
        assert_eq!(c.consume(Color::Black, 10_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 60_000);
        assert_eq!(c.consume(Color::Black, 2_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 63_000);
    }

    #[test]
    fn fischer_accepts_move_up_to_total_plus_increment_on_move_one() {
        // CSA client は move 1 で `total + increment` の budget を engine に配る
        // ので server もそれを受理する。例: total=60, inc=5 → move 1 は 65 秒まで
        // 受理、66 秒で TimeUp。
        let mut c = FischerClock::new(60, 5);
        assert_eq!(c.consume(Color::Black, 65_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 5_000);
        // 次手の残時間は 5 秒。5 秒使えば OK、6 秒で TimeUp。
        let mut c2 = FischerClock::new(60, 5);
        assert_eq!(c2.consume(Color::Black, 66_000), ClockResult::TimeUp);
    }

    #[test]
    fn fischer_time_up_when_elapsed_exceeds_total_plus_increment() {
        // client の init = total + increment 規則。total=5, inc=3 → 初手 8 秒まで。
        // 9 秒で TimeUp。
        let mut c = FischerClock::new(5, 3);
        assert_eq!(c.consume(Color::Black, 9_000), ClockResult::TimeUp);
        assert_eq!(c.remaining_main_ms(Color::Black), 0);
    }

    #[test]
    fn fischer_consume_truncates_to_second() {
        // 999ms は 0 秒に切り捨て。init slot=65, consume(999): 65 - 0 + 5 = 70。
        let mut c = FischerClock::new(60, 5);
        assert_eq!(c.consume(Color::Black, 999), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 70_000);
    }

    #[test]
    fn fischer_format_summary_includes_increment_field() {
        let c = FischerClock::new(600, 10);
        let s = c.format_summary();
        assert!(s.contains("BEGIN Time"));
        assert!(s.contains("Time_Unit:1sec"));
        assert!(s.contains("Total_Time:600"));
        assert!(s.contains("Increment:10"));
        assert!(!s.contains("Byoyomi:"), "Fischer には Byoyomi フィールドを含めない");
        assert!(s.contains("Least_Time_Per_Move:0"));
        assert!(s.contains("END Time"));
    }

    #[test]
    fn fischer_turn_budget_uses_current_slot_plus_second_grain() {
        // init slot = total + inc = 65 秒。`turn_budget_ms` は slot + grain 999ms。
        let c = FischerClock::new(60, 5);
        assert_eq!(c.turn_budget_ms(Color::Black), 65_999);
    }

    #[test]
    fn fischer_black_and_white_are_independent() {
        let mut c = FischerClock::new(60, 5);
        assert_eq!(c.consume(Color::Black, 10_000), ClockResult::Continue);
        // init で両者とも 60+5=65 秒、Black だけ消費後 60 秒、White は 65 秒のまま。
        assert_eq!(c.remaining_main_ms(Color::White), 65_000);
    }

    // ---- StopWatchClock ----

    #[test]
    fn stopwatch_discards_sub_minute_consumption() {
        // 30 秒使っても分単位切り捨てで消費ゼロ。残量は初期値のまま。
        let mut c = StopWatchClock::new(10, 1);
        assert_eq!(c.consume(Color::Black, 30_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 10 * 60 * 1000);
        // 59 秒も同様。
        assert_eq!(c.consume(Color::Black, 59_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 10 * 60 * 1000);
    }

    #[test]
    fn stopwatch_consumes_minute_at_60_second_boundary() {
        // ちょうど 60 秒経過で 1 分消費される。
        let mut c = StopWatchClock::new(10, 1);
        assert_eq!(c.consume(Color::Black, 60_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 9 * 60 * 1000);
    }

    #[test]
    fn stopwatch_enters_byoyomi_when_main_exhausted() {
        // 本体 3 分 + 秒読み 2 分。3 分ちょうどで本体 0、秒読み区間に入る。
        let mut c = StopWatchClock::new(3, 2);
        assert_eq!(c.consume(Color::Black, 3 * 60_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::Black), 0);
        // 秒読み 2 分以内 (例: 119 秒 = 1 分消費) なら TimeUp ではない。
        assert_eq!(c.consume(Color::Black, 119_000), ClockResult::Continue);
    }

    #[test]
    fn stopwatch_time_up_when_over_byoyomi() {
        // 本体 1 分 + 秒読み 1 分。3 分経過で時間切れ (本体 1 + 秒読み 1 = 2 分を超過)。
        let mut c = StopWatchClock::new(1, 1);
        assert_eq!(c.consume(Color::Black, 3 * 60_000), ClockResult::TimeUp);
    }

    #[test]
    fn stopwatch_format_summary_uses_minute_unit() {
        // StopWatch は consume で分単位切り捨てを行うため、Game_Summary も
        // `Time_Unit:1min` で分単位宣言する。秒表記にすると `consume` の分切り
        // 捨てと `T<sec>` broadcast の秒単位が乖離して、client 側の remaining
        // が server 側と食い違う。
        let c = StopWatchClock::new(15, 1);
        let s = c.format_summary();
        assert!(s.contains("BEGIN Time"));
        assert!(s.contains("Time_Unit:1min"));
        assert!(s.contains("Total_Time:15"));
        assert!(s.contains("Byoyomi:1"));
        assert!(s.contains("Least_Time_Per_Move:0"));
        assert!(s.contains("END Time"));
    }

    #[test]
    fn stopwatch_turn_budget_includes_byoyomi_plus_minute_grain() {
        // 本体 15 分 + 秒読み 1 分 + minute grain (59_999ms) = 16 分 + 59_999ms。
        // `consume` が分単位に切り捨てる挙動と scheduler deadline を整合させるため、
        // 次の分境界の直前まで delay を伸ばす。これが無いと 1 分 byoyomi の局面で
        // scheduler が最大 59 秒早く TimeUp を発火させてしまう。
        let c = StopWatchClock::new(15, 1);
        assert_eq!(c.turn_budget_ms(Color::Black), 16 * 60_000 + 59_999);
    }

    #[test]
    fn stopwatch_black_and_white_are_independent() {
        let mut c = StopWatchClock::new(15, 1);
        assert_eq!(c.consume(Color::Black, 60_000), ClockResult::Continue);
        assert_eq!(c.remaining_main_ms(Color::White), 15 * 60 * 1000);
    }

    #[test]
    fn clock_spec_builds_matching_summary_for_countdown() {
        let spec = ClockSpec::Countdown {
            total_time_sec: 600,
            byoyomi_sec: 10,
        };
        assert_eq!(
            spec.format_time_section(),
            SecondsCountdownClock::new(600, 10).format_summary()
        );
    }

    #[test]
    fn clock_spec_builds_matching_summary_for_fischer() {
        let spec = ClockSpec::Fischer {
            total_time_sec: 600,
            increment_sec: 10,
        };
        assert_eq!(spec.format_time_section(), FischerClock::new(600, 10).format_summary());
    }

    #[test]
    fn clock_spec_builds_matching_summary_for_stopwatch() {
        let spec = ClockSpec::StopWatch {
            total_time_min: 15,
            byoyomi_min: 1,
        };
        assert_eq!(spec.format_time_section(), StopWatchClock::new(15, 1).format_summary());
    }
}
