//! 長い探索の途中経過を呼び出し側へ渡す口（`search-updates` feature）。
//!
//! 反復深化の `on_info` は深さを 1 段読み終えたときにしか呼ばれないため、深い段では
//! 数分間なにも届かない。[`Search::go_with_updates`](super::Search::go_with_updates) に
//! コールバックを渡すと、段の途中でも次の 3 種類の [`SearchUpdate`] を受け取れる。
//!
//! - 一定間隔の進み具合（深さ・局面数・経過時間・NPS）: [`SearchUpdate::Progress`]
//! - 根の手を読み始めたとき: [`SearchUpdate::CurrMove`]
//! - MultiPV の読み筋を 1 本読み終えたとき: [`SearchUpdate::PvLine`]
//!
//! いずれもメインスレッドの局面数が [`SearchUpdateOptions::min_nodes`] を超えてから出す。
//! 短い持ち時間の探索では何も呼ばれない。探索窓の外で返った値（fail high / low）の読み筋は
//! 値が確定していないので出さない。

use crate::types::{Depth, Move};

use super::SearchInfo;

/// Stockfish の `NODES_LIMIT_OUTPUT` と同じ既定のしきい値。
pub const DEFAULT_UPDATE_MIN_NODES: u64 = 10_000_000;

/// [`SearchUpdate::Progress`] の既定の間隔（ミリ秒）。
pub const DEFAULT_PROGRESS_INTERVAL_MS: u64 = 1000;

/// 途中経過を出す条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchUpdateOptions {
    /// メインスレッドの局面数がこの値を超えてから途中経過を出す。
    pub min_nodes: u64,
    /// [`SearchUpdate::Progress`] を出す間隔（ミリ秒）。実際の判定は停止確認
    /// （既定で 512 局面ごと）に合わせて行うため、間隔はその分だけ遅れることがある。
    pub progress_interval_ms: u64,
}

impl Default for SearchUpdateOptions {
    fn default() -> Self {
        Self {
            min_nodes: DEFAULT_UPDATE_MIN_NODES,
            progress_interval_ms: DEFAULT_PROGRESS_INTERVAL_MS,
        }
    }
}

/// 一定間隔の進み具合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchStatus {
    /// いま読んでいる反復の深さ
    pub depth: Depth,
    /// 全スレッドの探索局面数（補助スレッドの分は停止確認ごとに足し込まれた値）
    pub nodes: u64,
    /// 経過時間（ミリ秒）
    pub time_ms: u64,
    /// NPS (nodes per second)
    pub nps: u64,
}

/// 根の手を読み始めたことの通知。根の手を順に読むループで出し、根の ProbCut が
/// 手を試す分は含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrMove {
    /// いま読んでいる反復の深さ
    pub depth: Depth,
    /// 読み始めた根の手
    pub mv: Move,
    /// 何手目か（1 始まり）。MultiPV の 2 本目以降は確定済みの本の数を含めた通し番号
    /// （USI の `currmovenumber`・Stockfish と同じ）。その本の中での番号は
    /// `number - (multi_pv - 1)`。
    pub number: usize,
    /// いま読んでいる MultiPV の本（1 始まり）
    pub multi_pv: usize,
}

/// 探索の途中経過。
#[derive(Debug)]
pub enum SearchUpdate<'a> {
    /// 一定間隔の進み具合
    Progress(SearchStatus),
    /// 根の手を読み始めた
    CurrMove(CurrMove),
    /// MultiPV の読み筋を 1 本読み終えたときの、その時点で確定している本
    /// （`multi_pv` が 1 からその本まで）の値。反復の最後の本を読み終えたときは
    /// 従来どおり `on_info` に全部の本が渡るので、ここでは出さない。
    PvLine(&'a SearchInfo),
}

/// [`TimeManagement`](super::TimeManagement) に持たせる通知先。メインスレッドだけが持つ。
pub(crate) struct UpdateSink {
    options: SearchUpdateOptions,
    callback: Box<dyn FnMut(&SearchUpdate<'_>) + Send>,
    next_progress_ms: u64,
}

impl UpdateSink {
    pub(crate) fn new(
        options: SearchUpdateOptions,
        callback: Box<dyn FnMut(&SearchUpdate<'_>) + Send>,
    ) -> Self {
        Self {
            options,
            callback,
            next_progress_ms: options.progress_interval_ms,
        }
    }

    #[inline]
    pub(crate) fn active(&self, main_nodes: u64) -> bool {
        main_nodes > self.options.min_nodes
    }

    pub(crate) fn emit(&mut self, update: &SearchUpdate<'_>) {
        (self.callback)(update);
    }

    /// 次の [`SearchUpdate::Progress`] の時刻に達していれば、次の時刻を進めて `true` を返す。
    pub(crate) fn progress_due(&mut self, elapsed_ms: u64) -> bool {
        if elapsed_ms < self.next_progress_ms {
            return false;
        }
        self.next_progress_ms = elapsed_ms.saturating_add(self.options.progress_interval_ms.max(1));
        true
    }
}
