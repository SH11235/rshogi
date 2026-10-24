//! `--dispatch async`: 1 台内の非同期 per-pair 更新 (fishtest 流)。
//!
//! coordinator (main thread) が θ・pair の状態表・journal・snapshot・CSV を所有し、
//! 永続 worker へ pair を 1 つずつ渡す。各 pair は独立な摂動を持ち、結果が届いた順に
//! dispatch 時の係数 (c_k, r_k) で現在の θ を更新する。
//!
//! 永続化の正本は追記専用の `journal.jsonl` と、世代ごとの snapshot。θ への適用は
//! journal 上の `accept` の順で、通常実行と resume の再生は同じ `apply_accept` を使う。

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::*;

/// `meta.json` の async 用 format_version。batch の v4 とは別 schema。
pub(super) const ASYNC_META_FORMAT_VERSION: u32 = 5;
/// pair 単位の乱数は `compute_batch_prep` を batch_pairs=1・batch 番号=pair_id で呼んで作る。
const RNG_VERSION: &str = "batch-prep-per-pair-v1";
const JOURNAL_FILE: &str = "journal.jsonl";
const SNAPSHOT_DIR: &str = "snapshots";

/// f64 を Rust の最短 round-trip 表記の文字列で保存する。serde_json の既定の数値 parse は
/// 最下位 bit がずれ得るため、θ と engine 送信値は文字列経由で bit 一致を保つ。
mod exact_f64 {
    use serde::{Deserialize, Deserializer, Serializer, ser::SerializeSeq};

    pub fn serialize<S: Serializer>(values: &[f64], serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for value in values {
            seq.serialize_element(&format!("{value:?}"))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<f64>, D::Error> {
        Vec::<String>::deserialize(deserializer)?
            .iter()
            .map(|text| text.parse::<f64>().map_err(serde::de::Error::custom))
            .collect()
    }
}

/// 1 局の結果。`plus_score` は plus 視点 (+1 / 0 / -1)。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct GameRecord {
    plus_is_black: bool,
    plus_score: i8,
    reason: String,
    plies: u32,
}

/// pair の確定値。再 attempt と resume 後の指し直しは同じ値を使う。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct DispatchRecord {
    pair_id: u32,
    attempt: u32,
    dispatch_k: u32,
    start_pos_index: usize,
    /// active param の摂動符号 (`+` / `-`)。再生を乱数実装から切り離すために持つ。
    flips: String,
    /// active param の engine 送信値 (丸め・clamp 済み)。
    #[serde(with = "exact_f64")]
    plus: Vec<f64>,
    #[serde(with = "exact_f64")]
    minus: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JournalRecord {
    Dispatch(DispatchRecord),
    /// 結果の受信。適用 (`Accept`) とは別に記録し、保留中の結果も crash で失わない。
    Received {
        pair_id: u32,
        attempt: u32,
        games: [GameRecord; 2],
    },
    /// θ への適用。journal 上の順序が適用順になる。
    Accept {
        pair_id: u32,
    },
    AttemptFailed {
        pair_id: u32,
        attempt: u32,
        reason: String,
    },
    Duplicate {
        pair_id: u32,
        attempt: u32,
    },
}

/// 適用 1 回分の記録 (stats.csv の 1 行)。
#[derive(Clone, Debug)]
struct AppliedRow {
    completed_pairs: u32,
    pair_id: u32,
    attempt: u32,
    dispatch_k: u32,
    lag: u32,
    result: i8,
    games: [GameRecord; 2],
    active_params: usize,
    avg_abs_shift: f64,
    update: UpdateStats,
    /// 適用後の θ (values.csv の 1 行)。
    theta: Vec<f64>,
}

/// run 中に変わらない更新・乱数の入力。
#[derive(Clone, Copy)]
struct CoreEnv<'a> {
    schedule: ScheduleConfig,
    big_a: f64,
    param_schedules: &'a [ParamScheduleConstants],
    active_mask: &'a [bool],
    active_only_regex: Option<&'a Regex>,
    translator: &'a EngineNameTranslator,
    start_positions_len: usize,
    random_startpos: bool,
    base_seed: u64,
    total_pairs: u32,
    max_lag: u32,
    max_pair_attempts: u32,
}

/// dispatch 時の係数で現在の θ を更新する。通常実行と resume の再生で共有する純粋関数。
fn apply_accept(
    params: &mut [SpsaParam],
    env: &CoreEnv<'_>,
    dispatch: &DispatchRecord,
    games: &[GameRecord; 2],
    completed_before: u32,
) -> Result<AppliedRow> {
    let mut flips = vec![0.0; params.len()];
    let mut signs = dispatch.flips.chars();
    for (flip, _) in flips.iter_mut().zip(env.active_mask).filter(|(_, active)| **active) {
        *flip = match signs.next() {
            Some('+') => 1.0,
            Some('-') => -1.0,
            other => bail!("pair {}: invalid flip sign {other:?}", dispatch.pair_id),
        };
    }
    if signs.next().is_some() {
        bail!("pair {}: flips longer than active params", dispatch.pair_id);
    }
    let mut active_params = 0usize;
    let mut abs_shift_sum = 0.0f64;
    for ((sched, &flip), &active) in env.param_schedules.iter().zip(&flips).zip(env.active_mask) {
        if !active {
            continue;
        }
        let (c_k, _) = sched.at_iteration(
            dispatch.dispatch_k,
            env.big_a,
            env.schedule.alpha,
            env.schedule.gamma,
        );
        active_params += 1;
        abs_shift_sum += (c_k * flip).abs();
    }
    let result = games.iter().map(|game| game.plus_score).sum::<i8>();
    let update = apply_spsa_update(
        params,
        &flips,
        env.param_schedules,
        env.active_mask,
        env.schedule,
        dispatch.dispatch_k,
        f64::from(result),
    );
    Ok(AppliedRow {
        completed_pairs: completed_before + 1,
        pair_id: dispatch.pair_id,
        attempt: dispatch.attempt,
        dispatch_k: dispatch.dispatch_k,
        lag: completed_before - dispatch.dispatch_k,
        result,
        games: games.clone(),
        active_params,
        avg_abs_shift: if active_params > 0 {
            abs_shift_sum / active_params as f64
        } else {
            0.0
        },
        update,
        theta: params.iter().map(|p| p.value).collect(),
    })
}

/// 未確定 pair の dispatch_k (昇順) について、最古から順に適用すれば全 pair の lag が L 以下に
/// 収まるか。`C + i - k_i <= L` が全 i で成り立てば、最古の pair はいつでも適用できる。
fn lag_feasible(completed: u32, sorted_ks: &[u32], max_lag: u32) -> bool {
    sorted_ks
        .iter()
        .enumerate()
        .all(|(i, &k)| u64::from(completed) + i as u64 - u64::from(k) <= u64::from(max_lag))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairStatus {
    InFlight,
    RetryWait,
    Held,
}

#[derive(Clone, Debug)]
struct PairEntry {
    /// 最新 attempt の dispatch。確定値は全 attempt で同じ。
    dispatch: DispatchRecord,
    failures: u32,
    status: PairStatus,
    /// 現 attempt の dispatch 時点の受信総数。停滞の判定に使う。
    received_at_dispatch: u64,
    /// `Held` のときの結果。
    games: Option<[GameRecord; 2]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CoreCounters {
    /// worker に渡した attempt の数 (物理的に指し終えた数ではない)。
    dispatched_attempts: u64,
    failures: u64,
    duplicates: u64,
    received: u64,
    holds: u64,
    max_lag: u32,
}

/// coordinator が順に永続化・反映する出力。
#[derive(Debug)]
enum CoreOutput {
    Journal(JournalRecord),
    Applied(Box<AppliedRow>),
}

/// snapshot と再生結果の照合に使う状態。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CoreCheckpoint {
    completed_pairs: u32,
    next_pair_id: u32,
    failures: u64,
    received: u64,
    held: Vec<u32>,
    pending: Vec<PendingState>,
}

/// 未確定 pair 1 件の状態。snapshot と再生結果の照合用 (確定値は journal の dispatch と同じなので含めない)。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingState {
    pair_id: u32,
    status: String,
    attempt: u32,
    failures: u32,
    dispatch_k: u32,
    received_at_dispatch: u64,
    games: Option<[GameRecord; 2]>,
}

/// θ と pair の状態表。I/O を持たず、出力は `CoreOutput` として呼び出し側が journal・CSV に流す。
///
/// 不変条件:
/// - `completed + pairs.len() <= total_pairs` (`pairs` は未確定の pair だけを持つ)。
/// - 未確定 pair の dispatch_k が `lag_feasible` を満たす (適用時の lag <= L)。
struct AsyncCore<'a> {
    env: CoreEnv<'a>,
    params: Vec<SpsaParam>,
    completed: u32,
    next_pair_id: u32,
    pairs: BTreeMap<u32, PairEntry>,
    accepted: HashSet<u32>,
    /// 保留中の pair_id (受信順)。保留が解けたら先頭から再判定する。
    held: Vec<u32>,
    retry: BTreeSet<u32>,
    counters: CoreCounters,
}

impl<'a> AsyncCore<'a> {
    fn new(env: CoreEnv<'a>, params: Vec<SpsaParam>) -> Self {
        Self {
            env,
            params,
            completed: 0,
            next_pair_id: 0,
            pairs: BTreeMap::new(),
            accepted: HashSet::new(),
            held: Vec::new(),
            retry: BTreeSet::new(),
            counters: CoreCounters::default(),
        }
    }

    fn is_complete(&self) -> bool {
        self.completed == self.env.total_pairs
    }

    fn checkpoint(&self) -> CoreCheckpoint {
        CoreCheckpoint {
            completed_pairs: self.completed,
            next_pair_id: self.next_pair_id,
            failures: self.counters.failures,
            received: self.counters.received,
            held: self.held.clone(),
            pending: self
                .pairs
                .iter()
                .map(|(&pair_id, entry)| PendingState {
                    pair_id,
                    // resume は中断した attempt を journal に書かずに retry_wait へ移すので、
                    // 結果待ちの 2 状態は区別せずに照合する。
                    status: if entry.status == PairStatus::Held {
                        "held"
                    } else {
                        "unreceived"
                    }
                    .into(),
                    attempt: entry.dispatch.attempt,
                    failures: entry.failures,
                    dispatch_k: entry.dispatch.dispatch_k,
                    received_at_dispatch: entry.received_at_dispatch,
                    games: entry.games.clone(),
                })
                .collect(),
        }
    }

    /// 失敗数が上限に達したまま次の attempt を待つ pair があれば停止理由を返す。
    /// 上限を引き上げて resume したときだけ指し直しを続ける。
    fn exhausted_retry(&self) -> Option<String> {
        self.retry.iter().find_map(|pair_id| {
            let failures = self.pairs[pair_id].failures;
            (failures >= self.env.max_pair_attempts).then(|| {
                format!(
                    "pair {pair_id} failed {failures} attempt(s) (limit {})",
                    self.env.max_pair_attempts
                )
            })
        })
    }

    fn sorted_ks(&self, skip: Option<u32>) -> Vec<u32> {
        let mut ks: Vec<u32> = self
            .pairs
            .iter()
            .filter(|(id, _)| Some(**id) != skip)
            .map(|(_, entry)| entry.dispatch.dispatch_k)
            .collect();
        ks.sort_unstable();
        ks
    }

    /// 次に worker へ渡す pair。再 attempt を新規 pair より優先する。
    /// 新規 pair は予算と lag の不変条件を満たすときだけ作る。
    fn next_dispatch(&mut self) -> Result<Option<DispatchRecord>> {
        if let Some(pair_id) = self.retry.pop_first() {
            let received = self.counters.received;
            let entry = self.pairs.get_mut(&pair_id).context("retry pair is missing")?;
            entry.dispatch.attempt += 1;
            entry.status = PairStatus::InFlight;
            entry.received_at_dispatch = received;
            self.counters.dispatched_attempts += 1;
            return Ok(Some(entry.dispatch.clone()));
        }
        if self.completed as usize + self.pairs.len() >= self.env.total_pairs as usize {
            return Ok(None);
        }
        let mut ks = self.sorted_ks(None);
        ks.push(self.completed);
        if !lag_feasible(self.completed, &ks, self.env.max_lag) {
            return Ok(None);
        }
        let pair_id = self.next_pair_id;
        let ctx = BatchPrepCtx {
            big_a: self.env.big_a,
            schedule: self.env.schedule,
            params: &self.params,
            param_schedules: self.env.param_schedules,
            active_only_regex: self.env.active_only_regex,
            translator: self.env.translator,
            start_positions_len: self.env.start_positions_len,
            batch_pairs: 1,
            random_startpos: self.env.random_startpos,
        };
        // batch_pairs=1・batch 番号=pair_id の batch と同じ乱数列になる。
        // 開始局面の周回は total_games_start / 2 = pair_id で進む。
        let games_start = usize::try_from(u64::from(pair_id) * 2).context("game index overflow")?;
        let prep =
            compute_batch_prep(&ctx, pair_id, self.completed, self.env.base_seed, games_start)?;
        let mut flips = String::new();
        let mut plus = Vec::new();
        let mut minus = Vec::new();
        for (idx, &active) in self.env.active_mask.iter().enumerate() {
            if active {
                flips.push(if prep.flips[idx] > 0.0 { '+' } else { '-' });
                plus.push(prep.plus_values[idx]);
                minus.push(prep.minus_values[idx]);
            }
        }
        let dispatch = DispatchRecord {
            pair_id,
            attempt: 1,
            dispatch_k: self.completed,
            start_pos_index: prep.start_pos_indices[0],
            flips,
            plus,
            minus,
        };
        self.insert_new(dispatch.clone())?;
        Ok(Some(dispatch))
    }

    fn insert_new(&mut self, dispatch: DispatchRecord) -> Result<()> {
        self.next_pair_id = dispatch.pair_id.checked_add(1).context("pair_id overflow")?;
        self.counters.dispatched_attempts += 1;
        self.pairs.insert(
            dispatch.pair_id,
            PairEntry {
                dispatch,
                failures: 0,
                status: PairStatus::InFlight,
                received_at_dispatch: self.counters.received,
                games: None,
            },
        );
        Ok(())
    }

    /// worker へ渡す全 param 長の送信値。固定項目は batch と同じ固定値になる。
    fn engine_values(&self, dispatch: &DispatchRecord) -> (Vec<f64>, Vec<f64>) {
        let mut plus = Vec::with_capacity(self.params.len());
        let mut minus = Vec::with_capacity(self.params.len());
        let mut active_index = 0;
        for (p, &active) in self.params.iter().zip(self.env.active_mask) {
            if active {
                plus.push(dispatch.plus[active_index]);
                minus.push(dispatch.minus[active_index]);
                active_index += 1;
            } else {
                let fixed = fixed_engine_value(p);
                plus.push(fixed);
                minus.push(fixed);
            }
        }
        (plus, minus)
    }

    /// 結果を受け取る。実行中の現 attempt 以外の結果は duplicate として記録だけ残す。
    fn on_done(
        &mut self,
        pair_id: u32,
        attempt: u32,
        games: [GameRecord; 2],
    ) -> Result<Vec<CoreOutput>> {
        let current = self.pairs.get(&pair_id).is_some_and(|entry| {
            entry.status == PairStatus::InFlight && entry.dispatch.attempt == attempt
        });
        if !current {
            if !self.accepted.contains(&pair_id) && !self.pairs.contains_key(&pair_id) {
                bail!("result for unknown pair {pair_id}");
            }
            self.counters.duplicates += 1;
            return Ok(vec![CoreOutput::Journal(JournalRecord::Duplicate {
                pair_id,
                attempt,
            })]);
        }
        let record = JournalRecord::Received {
            pair_id,
            attempt,
            games,
        };
        self.receive(&record)?;
        let mut out = vec![CoreOutput::Journal(record)];
        self.apply_ready(&mut out)?;
        if self.held.contains(&pair_id) {
            self.counters.holds += 1;
        }
        Ok(out)
    }

    fn receive(&mut self, record: &JournalRecord) -> Result<()> {
        let JournalRecord::Received {
            pair_id,
            attempt,
            games,
        } = record
        else {
            bail!("receive expects a received record");
        };
        let entry = self
            .pairs
            .get_mut(pair_id)
            .filter(|entry| {
                entry.status == PairStatus::InFlight && entry.dispatch.attempt == *attempt
            })
            .with_context(|| {
                format!("journal: unexpected result for pair {pair_id} attempt {attempt}")
            })?;
        entry.status = PairStatus::Held;
        entry.games = Some(games.clone());
        self.held.push(*pair_id);
        self.counters.received += 1;
        Ok(())
    }

    /// 失敗した attempt を記録する。失敗数が上限に達したら停止理由を返す。
    fn on_failed(
        &mut self,
        pair_id: u32,
        attempt: u32,
        reason: String,
    ) -> (CoreOutput, Option<String>) {
        let current = self.pairs.get(&pair_id).is_some_and(|entry| {
            entry.status == PairStatus::InFlight && entry.dispatch.attempt == attempt
        });
        if !current {
            self.counters.duplicates += 1;
            return (CoreOutput::Journal(JournalRecord::Duplicate { pair_id, attempt }), None);
        }
        let stop = self
            .record_failure(pair_id, attempt)
            .filter(|failures| *failures >= self.env.max_pair_attempts)
            .map(|failures| {
                format!(
                    "pair {pair_id} failed {failures} attempt(s) (limit {})",
                    self.env.max_pair_attempts
                )
            });
        let record = JournalRecord::AttemptFailed {
            pair_id,
            attempt,
            reason,
        };
        (CoreOutput::Journal(record), stop)
    }

    /// 実行中の現 attempt の失敗だけを数え、次の attempt 待ちにする。
    fn record_failure(&mut self, pair_id: u32, attempt: u32) -> Option<u32> {
        let entry = self.pairs.get_mut(&pair_id)?;
        if entry.status != PairStatus::InFlight || entry.dispatch.attempt != attempt {
            return None;
        }
        entry.failures += 1;
        entry.status = PairStatus::RetryWait;
        self.retry.insert(pair_id);
        self.counters.failures += 1;
        Some(entry.failures)
    }

    /// 保留中の結果のうち、lag の不変条件を保てるものを受信順に適用する。
    fn apply_ready(&mut self, out: &mut Vec<CoreOutput>) -> Result<()> {
        while let Some(index) = self.held.iter().position(|&pair_id| self.can_apply(pair_id)) {
            let pair_id = self.held.remove(index);
            out.push(CoreOutput::Journal(JournalRecord::Accept { pair_id }));
            let row = self.accept(pair_id)?;
            out.push(CoreOutput::Applied(Box::new(row)));
        }
        Ok(())
    }

    fn can_apply(&self, pair_id: u32) -> bool {
        lag_feasible(self.completed + 1, &self.sorted_ks(Some(pair_id)), self.env.max_lag)
    }

    fn accept(&mut self, pair_id: u32) -> Result<AppliedRow> {
        let entry = self
            .pairs
            .remove(&pair_id)
            .filter(|entry| entry.status == PairStatus::Held)
            .with_context(|| format!("journal: accept for pair {pair_id} without result"))?;
        let games = entry.games.as_ref().context("held pair without result")?;
        let row =
            apply_accept(&mut self.params, &self.env, &entry.dispatch, games, self.completed)?;
        self.completed += 1;
        self.accepted.insert(pair_id);
        self.counters.max_lag = self.counters.max_lag.max(row.lag);
        Ok(row)
    }

    /// 予算は残っているのに lag の不変条件で新規 dispatch できず、保留もあるまま、最古の未確定 pair が
    /// 現 attempt の dispatch 後に L 件以上の結果に追い越されていたら停止理由を返す。
    /// 予算の末尾で worker が空くのは正常なので止めない。resume で出し直した attempt は数え直す。
    fn stall_reason(&self) -> Option<String> {
        let budget_left =
            (self.completed as usize + self.pairs.len()) < self.env.total_pairs as usize;
        if self.held.is_empty() || !budget_left {
            return None;
        }
        let mut ks = self.sorted_ks(None);
        ks.push(self.completed);
        if lag_feasible(self.completed, &ks, self.env.max_lag) {
            return None;
        }
        let (pair_id, oldest) = self
            .pairs
            .iter()
            .filter(|(_, entry)| entry.status != PairStatus::Held)
            .min_by_key(|(id, entry)| (entry.dispatch.dispatch_k, **id))?;
        let overtaken = self.counters.received - oldest.received_at_dispatch;
        (overtaken >= u64::from(self.env.max_lag)).then(|| {
            format!(
                "lag stall: pair {pair_id} (dispatch_k={}, attempt {}) was overtaken by {overtaken} results \
                 while {} result(s) are held (L={})",
                oldest.dispatch.dispatch_k,
                oldest.dispatch.attempt,
                self.held.len(),
                self.env.max_lag
            )
        })
    }

    /// journal の 1 record を再生する。θ の更新は通常実行と同じ `apply_accept`。
    fn replay(&mut self, record: &JournalRecord) -> Result<Option<AppliedRow>> {
        match record {
            JournalRecord::Dispatch(dispatch) => {
                let pair_id = dispatch.pair_id;
                if self.accepted.contains(&pair_id) {
                    bail!("journal: dispatch for accepted pair {pair_id}");
                }
                if let Some(entry) = self.pairs.get_mut(&pair_id) {
                    // 失敗した attempt (retry_wait) か、coordinator の停止で中断した attempt
                    // (in_flight のまま次の dispatch が来る) だけを置き換える。確定値は同じであること。
                    let mut expected = entry.dispatch.clone();
                    expected.attempt += 1;
                    if entry.status == PairStatus::Held || *dispatch != expected {
                        bail!("journal: unexpected re-dispatch of pair {pair_id}");
                    }
                    entry.dispatch.attempt = dispatch.attempt;
                    entry.status = PairStatus::InFlight;
                    entry.received_at_dispatch = self.counters.received;
                    self.retry.remove(&pair_id);
                    self.counters.dispatched_attempts += 1;
                    return Ok(None);
                }
                if pair_id != self.next_pair_id
                    || dispatch.attempt != 1
                    || dispatch.dispatch_k != self.completed
                {
                    bail!(
                        "journal: unexpected new pair {pair_id} attempt {} dispatch_k {}",
                        dispatch.attempt,
                        dispatch.dispatch_k
                    );
                }
                if self.completed as usize + self.pairs.len() >= self.env.total_pairs as usize {
                    bail!("journal: dispatch beyond total_pairs");
                }
                self.insert_new(dispatch.clone())?;
                Ok(None)
            }
            JournalRecord::Received { .. } => {
                self.receive(record)?;
                Ok(None)
            }
            JournalRecord::Accept { pair_id } => {
                let index =
                    self.held.iter().position(|held| held == pair_id).with_context(|| {
                        format!("journal: accept for pair {pair_id} without result")
                    })?;
                self.held.remove(index);
                Ok(Some(self.accept(*pair_id)?))
            }
            JournalRecord::AttemptFailed {
                pair_id, attempt, ..
            } => {
                if self.record_failure(*pair_id, *attempt).is_none() {
                    bail!(
                        "journal: attempt_failed for pair {pair_id} attempt {attempt} not in flight"
                    );
                }
                Ok(None)
            }
            JournalRecord::Duplicate { pair_id, .. } => {
                if !self.accepted.contains(pair_id) && !self.pairs.contains_key(pair_id) {
                    bail!("journal: duplicate for unknown pair {pair_id}");
                }
                self.counters.duplicates += 1;
                Ok(None)
            }
        }
    }

    /// 再生の後、実行中だった pair を失敗数を増やさず次の attempt 待ちにし、
    /// 適用できる保留を適用する。
    fn finish_replay(&mut self) -> Result<Vec<CoreOutput>> {
        for (&pair_id, entry) in &mut self.pairs {
            if entry.status == PairStatus::InFlight {
                entry.status = PairStatus::RetryWait;
                self.retry.insert(pair_id);
            }
        }
        let mut out = Vec::new();
        self.apply_ready(&mut out)?;
        Ok(out)
    }
}

// =============================================================================
// worker
// =============================================================================

struct PairTask {
    pair_id: u32,
    attempt: u32,
    start_pos_index: usize,
    plus: Vec<f64>,
    minus: Vec<f64>,
}

#[derive(Debug)]
enum WorkerMessage {
    Done {
        pair_id: u32,
        attempt: u32,
        games: [GameRecord; 2],
    },
    /// `--engine-retries` を使い切った infra 失敗。coordinator が次の attempt を出す。
    Failed {
        pair_id: u32,
        attempt: u32,
        reason: String,
    },
}

/// 同じ設定値のまま先後を入れ替えて 2 局指す。どちらかの局で失敗したら pair 全体を失敗にする。
fn play_pair(
    ctx: WorkerContext<'_>,
    task: &PairTask,
    plus: &mut EngineProcess,
    minus: &mut EngineProcess,
) -> Result<[GameRecord; 2]> {
    use tools::selfplay::game::EngineFailure;
    apply_parameter_vector(plus, ctx.params, &task.plus, ctx.translator, ctx.application_mask)
        .context(EngineFailure)?;
    apply_parameter_vector(minus, ctx.params, &task.minus, ctx.translator, ctx.application_mask)
        .context(EngineFailure)?;
    let first_game_id = task.pair_id.checked_mul(2).context("game id overflow")?;
    let mut games = Vec::with_capacity(2);
    for (offset, plus_is_black) in [(1u32, true), (2, false)] {
        plus.new_game().context(EngineFailure)?;
        minus.new_game().context(EngineFailure)?;
        let (black, white) = if plus_is_black {
            (&mut *plus, &mut *minus)
        } else {
            (&mut *minus, &mut *plus)
        };
        let result = run_game(
            black,
            white,
            &ctx.start_positions[task.start_pos_index],
            ctx.tc,
            ctx.game_cfg,
            first_game_id.checked_add(offset).context("game id overflow")?,
            &mut |_: &MoveEvent| {},
            None,
        )?;
        if result.reason == "watchdog" {
            return Err(anyhow::anyhow!("watchdog: 運用上の探索期限超過").context(EngineFailure));
        }
        if result.reason == "cancelled" {
            return Err(WorkerCancelled.into());
        }
        games.push(GameRecord {
            plus_is_black,
            plus_score: plus_score_from_outcome(result.outcome, plus_is_black) as i8,
            reason: result.reason,
            plies: result.plies,
        });
    }
    let second = games.pop().context("pair must have two games")?;
    let first = games.pop().context("pair must have two games")?;
    Ok([first, second])
}

fn async_worker_loop(
    ctx: WorkerContext<'_>,
    idx: usize,
    tasks: crossbeam_channel::Receiver<PairTask>,
    results: &crossbeam_channel::Sender<Result<WorkerMessage>>,
    cancel: &AtomicBool,
    retries: u32,
    faults: &FaultPlan,
) -> Result<()> {
    use tools::selfplay::game::EngineFailure;
    let mut engines = None;
    // engine より後に宣言し、致命的エラーでは engine の破棄より先に他 worker を止める。
    let mut guard = WorkerCancelGuard {
        cancel,
        armed: true,
    };
    let result = (|| {
        for task in tasks {
            let mut failure = None;
            for try_index in 0..=retries {
                if cancel.load(Ordering::Acquire) {
                    return Ok(());
                }
                let played = (|| {
                    if engines.is_none() {
                        engines = Some(spawn_worker_engines(ctx, idx, cancel, false)?);
                    }
                    let (plus, minus) = engines.as_mut().context("missing worker engines")?;
                    play_pair(ctx, &task, plus, minus)
                })();
                match played {
                    Ok(games) => {
                        failure = None;
                        let sends = if faults.duplicates(task.pair_id) {
                            2
                        } else {
                            1
                        };
                        for _ in 0..sends {
                            let message = WorkerMessage::Done {
                                pair_id: task.pair_id,
                                attempt: task.attempt,
                                games: games.clone(),
                            };
                            if results.send(Ok(message)).is_err() {
                                return Ok(());
                            }
                        }
                        break;
                    }
                    Err(error) if error.downcast_ref::<EngineFailure>().is_some() => {
                        drop(engines.take());
                        if cancel.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        let cause = format!("{error:#}").replace(['\r', '\n'], " ");
                        if try_index < retries {
                            eprintln!(
                                "retry pair={} attempt={} try={} cause={cause}",
                                task.pair_id,
                                task.attempt,
                                u64::from(try_index) + 2
                            );
                        }
                        failure = Some(cause);
                    }
                    Err(error) => {
                        return Err(error
                            .context(format!("pair={} attempt={}", task.pair_id, task.attempt)));
                    }
                }
            }
            if let Some(reason) = failure {
                let message = WorkerMessage::Failed {
                    pair_id: task.pair_id,
                    attempt: task.attempt,
                    reason,
                };
                if results.send(Ok(message)).is_err() {
                    return Ok(());
                }
            }
        }
        Ok(())
    })();
    let result = match result {
        Err(error)
            if cancel.load(Ordering::Acquire)
                && error.downcast_ref::<WorkerCancelled>().is_some() =>
        {
            Ok(())
        }
        result => result,
    };
    if result.is_ok() {
        guard.armed = false;
    }
    result
}

struct AsyncPool<'scope> {
    tasks: Option<crossbeam_channel::Sender<PairTask>>,
    results: crossbeam_channel::Receiver<Result<WorkerMessage>>,
    cancel: Arc<AtomicBool>,
    handles: Vec<std::thread::ScopedJoinHandle<'scope, ()>>,
}

impl<'scope> AsyncPool<'scope> {
    fn new<'env: 'scope>(
        scope: &'scope std::thread::Scope<'scope, 'env>,
        count: usize,
        ctx: WorkerContext<'scope>,
        retries: u32,
        faults: &'scope FaultPlan,
    ) -> Result<Self> {
        let (tasks, task_rx) = unbounded::<PairTask>();
        let (result_tx, results) = unbounded();
        let cancel = ctx
            .game_cfg
            .cancel
            .clone()
            .context("worker pool requires a shared cancel flag")?;
        let mut handles = Vec::new();
        for idx in 0..count {
            let task_rx = task_rx.clone();
            let result_tx = result_tx.clone();
            let cancel = cancel.clone();
            handles.push(scope.spawn(move || {
                // panic も結果 channel へ通知し、coordinator の受信待ちを終わらせる。
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    async_worker_loop(ctx, idx, task_rx, &result_tx, &cancel, retries, faults)
                }));
                let error = match result {
                    Ok(Ok(())) => return,
                    Ok(Err(error)) => error,
                    Err(_) => anyhow::anyhow!("worker panic"),
                };
                cancel.store(true, Ordering::Release);
                let _ = result_tx.send(Err(error));
            }));
        }
        Ok(Self {
            tasks: Some(tasks),
            results,
            cancel,
            handles,
        })
    }

    fn send(&self, task: PairTask) -> Result<()> {
        self.tasks
            .as_ref()
            .context("worker pool closed")?
            .send(task)
            .context("failed to dispatch pair task")
    }

    fn recv(&self) -> Result<Result<WorkerMessage>> {
        self.results.recv().context("failed to receive pair result from worker")
    }

    fn try_recv(&self) -> Option<Result<WorkerMessage>> {
        self.results.try_recv().ok()
    }
}

impl Drop for AsyncPool<'_> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        drop(self.tasks.take());
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

// =============================================================================
// fault injection (テスト専用)
// =============================================================================

/// `RSHOGI_SPSA_FAULT=<point>:<n>` で n 回目の通過時に process を終了させ、crash 境界からの
/// resume を結合テストで再現する。`duplicate_result:<pair_id>` はその pair の結果を 2 度送る。
struct FaultPlan {
    point: Option<(String, u32)>,
    hits: std::sync::atomic::AtomicU32,
}

impl FaultPlan {
    fn from_env() -> Result<Self> {
        let point = match std::env::var("RSHOGI_SPSA_FAULT") {
            Ok(text) => {
                let (name, n) =
                    text.split_once(':').context("RSHOGI_SPSA_FAULT must be <point>:<n>")?;
                Some((name.to_owned(), n.parse().context("RSHOGI_SPSA_FAULT count")?))
            }
            Err(_) => None,
        };
        Ok(Self {
            point,
            hits: std::sync::atomic::AtomicU32::new(0),
        })
    }

    fn hit(&self, name: &str) {
        if let Some((point, n)) = &self.point
            && point == name
            && self.hits.fetch_add(1, Ordering::AcqRel) + 1 == *n
        {
            eprintln!("fault injection: exit at {name}");
            std::process::exit(86);
        }
    }

    /// n 回目の通過で I/O エラーと同じく Err を返す (write・sync の失敗経路の試験用)。
    fn fail(&self, name: &str) -> Result<()> {
        if let Some((point, n)) = &self.point
            && point == name
            && self.hits.fetch_add(1, Ordering::AcqRel) + 1 == *n
        {
            bail!("fault injection: {name}");
        }
        Ok(())
    }

    fn duplicates(&self, pair_id: u32) -> bool {
        matches!(&self.point, Some((point, n)) if point == "duplicate_result" && *n == pair_id)
    }
}

// =============================================================================
// 永続化: journal / snapshot / meta v5 / CSV
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RunStatus {
    Running,
    Completed,
    Stopped,
}

/// resume で一致を要求する対局条件。実行環境だけに依存する値 (並列数・期限) は含めない。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct RunFingerprint {
    engine_sha256: String,
    engine_args: Vec<String>,
    /// 既定値の注入後の USI option。
    usi_options: Vec<String>,
    /// 値がファイルを指す USI option (EvalFile 等) の内容の sha256。
    usi_option_files: Vec<(String, String)>,
    engine_param_mapping_sha256: Option<String>,
    startpos_sha256: Option<String>,
    sfen: Option<String>,
    random_startpos: bool,
    nodes: Option<u64>,
    byoyomi: u64,
    btime: Option<u64>,
    binc: u64,
    threads: usize,
    hash_mb: u32,
    max_moves: u32,
    nodes_timeout_ms: u64,
    timeout_margin_ms: u64,
}

impl RunFingerprint {
    fn new(cli: &Cli, engine: &EngineConfig) -> Result<Self> {
        let mut usi_option_files = Vec::new();
        for option in &engine.usi_options {
            // engine の初期化と同じく前後の空白を除いた値で判定する。
            if let Some((name, value)) = option.split_once('=')
                && Path::new(value.trim()).is_file()
            {
                let sha = sha256_hex_of_file(Path::new(value.trim()))?;
                usi_option_files.push((name.trim().to_owned(), sha));
            }
        }
        Ok(Self {
            engine_sha256: sha256_hex_of_file(&engine.path)?,
            engine_args: engine.args.clone(),
            usi_options: engine.usi_options.clone(),
            usi_option_files,
            engine_param_mapping_sha256: cli
                .engine_param_mapping
                .as_deref()
                .map(sha256_hex_of_file)
                .transpose()?,
            startpos_sha256: cli.startpos_file.as_deref().map(sha256_hex_of_file).transpose()?,
            sfen: cli.sfen.clone(),
            random_startpos: cli.random_startpos,
            nodes: cli.nodes,
            byoyomi: cli.byoyomi,
            btime: cli.btime,
            binc: cli.binc,
            threads: cli.threads,
            hash_mb: cli.hash_mb,
            max_moves: cli.max_moves,
            nodes_timeout_ms: cli.nodes_timeout_ms,
            timeout_margin_ms: cli.timeout_margin_ms,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AsyncMeta {
    format_version: u32,
    dispatch: String,
    rng_version: String,
    /// 更新式・乱数列の実装を固定するための spsa binary の sha256。
    coordinator_sha256: String,
    base_seed: u64,
    total_pairs: u32,
    schedule: ScheduleConfig,
    max_lag: u32,
    /// 順序付きの param 定義と active・application の印の sha256。
    param_definition_sha256: String,
    active_param_count: usize,
    fingerprint: RunFingerprint,
    init_params_sha256: String,
    init_from_sha256: Option<String>,
    init_from_path: Option<String>,
    engine_path: String,
    engine_param_mapping_path: Option<String>,
    engine_param_mapping_sha256: Option<String>,
    init_mode: InitMode,
    /// 正本の snapshot (`snapshots/` 内のファイル名)。
    snapshot: String,
    completed_pairs: u32,
    status: RunStatus,
    stop_reason: Option<String>,
    dispatched_attempts: u64,
    failed_attempts: u64,
    duplicates: u64,
    holds: u64,
    max_observed_lag: u32,
    updated_at_utc: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Snapshot {
    generation: u64,
    /// この状態に反映済みの journal の byte 位置 (record の境界)。
    journal_offset: u64,
    checkpoint: CoreCheckpoint,
    param_names: Vec<String>,
    #[serde(with = "exact_f64")]
    theta: Vec<f64>,
    /// gen0 だけが持つ param 定義の正本。`state.params` は値を使わない view。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    definitions: Option<Vec<ParamDefinition>>,
    /// gen0 だけが持つ行末コメント。`state.params` を作り直すときに使う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comments: Option<Vec<String>>,
}

fn snapshot_file_name(generation: u64) -> String {
    format!("snapshot-{generation:06}.json")
}

/// 新しい task の値と再生の両方に効く param 定義。数値は最短 round-trip 表記で持つ。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ParamDefinition {
    name: String,
    type_name: String,
    min: String,
    max: String,
    c_end: String,
    r_end: String,
    not_used: bool,
    active: bool,
    applicable: bool,
}

fn param_definitions(params: &[SpsaParam], masks: &ParamMasks) -> Vec<ParamDefinition> {
    params
        .iter()
        .zip(&masks.active)
        .zip(&masks.application)
        .map(|((p, &active), &applicable)| ParamDefinition {
            name: p.name.clone(),
            type_name: p.type_name.clone(),
            min: format!("{:?}", p.min),
            max: format!("{:?}", p.max),
            c_end: format!("{:?}", p.c_end),
            r_end: format!("{:?}", p.r_end),
            not_used: p.not_used,
            active,
            applicable,
        })
        .collect()
}

fn param_definition_sha256(definitions: &[ParamDefinition]) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(definitions)?)))
}

/// rename の永続化に親 dir の fsync が要る (Linux)。std では Windows の dir handle を開けないため、
/// Windows は process crash までを保証範囲にする。
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("failed to fsync directory {}", dir.display()))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// 一時ファイル → fsync → rename → dir fsync の順で置き換える。
fn write_json_durable<T: Serialize>(path: &Path, value: &T, pretty: bool) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".spsa_")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("failed to create temp file under {}", parent.display()))?;
    {
        let mut writer = BufWriter::new(tmp.as_file_mut());
        if pretty {
            serde_json::to_writer_pretty(&mut writer, value)?;
        } else {
            serde_json::to_writer(&mut writer, value)?;
        }
        writer.flush().with_context(|| format!("failed to flush {}", path.display()))?;
    }
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("failed to fsync {}", path.display()))?;
    tmp.persist(path)
        .with_context(|| format!("failed to atomic-rename to {}", path.display()))?;
    sync_dir(parent)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {}", path.display()))
}

/// 追記専用の journal。各 record を書いてから `sync_data` する。
struct Journal {
    file: File,
    offset: u64,
}

impl Journal {
    fn create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        file.sync_all()?;
        sync_dir(path.parent().unwrap_or(Path::new(".")))?;
        Self::open_at(path, 0)
    }

    /// 再生で確かめた長さまで切り詰めて追記用に開く。
    fn open_at(path: &Path, len: u64) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        if file.metadata()?.len() != len {
            eprintln!("journal: discarding torn tail of {} after byte {len}", path.display());
            file.set_len(len)?;
            file.sync_all()?;
        }
        drop(file);
        let file = OpenOptions::new()
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {} for append", path.display()))?;
        Ok(Self { file, offset: len })
    }

    fn append(&mut self, record: &JournalRecord) -> Result<()> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        self.file.write_all(line.as_bytes()).context("failed to append journal")?;
        self.file.sync_data().context("failed to fsync journal")?;
        self.offset += line.len() as u64;
        Ok(())
    }
}

/// journal を先頭から逐次読み、record ごとに開始位置と共に渡す。改行で終わらない末尾の断片だけを
/// 書き込み途中の残骸として捨てる (record は 1 回の write で改行まで書く)。返り値は有効な長さ。
fn read_journal(
    path: &Path,
    mut on_record: impl FnMut(u64, &JournalRecord) -> Result<()>,
) -> Result<u64> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut offset = 0u64;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        let record = serde_json::from_slice::<JournalRecord>(&line[..read - 1])
            .with_context(|| format!("journal {} is corrupt at byte {offset}", path.display()))?;
        on_record(offset, &record)?;
        offset += read as u64;
    }
    Ok(offset)
}

fn plus_outcome_label(plus_score: i8) -> &'static str {
    match plus_score.signum() {
        1 => "plus_win",
        -1 => "minus_win",
        _ => "draw",
    }
}

fn write_stats_header(writer: &mut BufWriter<File>) -> Result<()> {
    writeln!(
        writer,
        "completed_pairs,pair_id,attempt,dispatch_k,lag,result,\
         g1_plus_is_black,g1_outcome,g1_reason,g1_plies,\
         g2_plus_is_black,g2_outcome,g2_reason,g2_plies,\
         active_params,avg_abs_shift,updated_params,avg_abs_update,max_abs_update"
    )?;
    Ok(())
}

fn write_stats_row(writer: &mut BufWriter<File>, row: &AppliedRow) -> Result<()> {
    write!(
        writer,
        "{},{},{},{},{},{:+}",
        row.completed_pairs, row.pair_id, row.attempt, row.dispatch_k, row.lag, row.result
    )?;
    for game in &row.games {
        write!(
            writer,
            ",{},{},{},{}",
            game.plus_is_black,
            plus_outcome_label(game.plus_score),
            game.reason,
            game.plies
        )?;
    }
    writeln!(
        writer,
        ",{},{:.6},{},{:.6},{:.6}",
        row.active_params,
        row.avg_abs_shift,
        row.update.updated_params,
        row.update.avg_abs_update,
        row.update.max_abs_update
    )?;
    Ok(())
}

fn write_values_header(writer: &mut BufWriter<File>, params: &[SpsaParam]) -> Result<()> {
    write!(writer, "completed_pairs")?;
    for param in params {
        write!(writer, ",{}", param.name)?;
    }
    writeln!(writer)?;
    Ok(())
}

fn write_values_row(writer: &mut BufWriter<File>, completed: u32, theta: &[f64]) -> Result<()> {
    write!(writer, "{completed}")?;
    // batch の values.csv と同じ 6 桁表示。正本は snapshot。
    for value in theta {
        write!(writer, ",{value:.6}")?;
    }
    writeln!(writer)?;
    Ok(())
}

/// journal から作る派生 CSV。append しながら flush するだけで fsync はしない。
struct CsvOutputs {
    stats: Option<(PathBuf, BufWriter<File>)>,
    values: Option<(PathBuf, BufWriter<File>)>,
}

impl CsvOutputs {
    /// 一時ファイルに header と θ0 の行を書き始める。`commit` で本来の名前へ置き換える。
    fn begin(cli: &Cli, params: &[SpsaParam]) -> Result<Self> {
        let open = |path: PathBuf| -> Result<(PathBuf, BufWriter<File>)> {
            let tmp = path.with_extension("csv.tmp");
            let file = File::create(&tmp)
                .with_context(|| format!("failed to create {}", tmp.display()))?;
            Ok((path, BufWriter::new(file)))
        };
        let mut stats = (!cli.no_stats_csv)
            .then(|| open(default_stats_csv_path(&cli.run_dir)))
            .transpose()?;
        let mut values = (!cli.no_param_values_csv)
            .then(|| open(default_param_values_csv_path(&cli.run_dir)))
            .transpose()?;
        if let Some((_, writer)) = stats.as_mut() {
            write_stats_header(writer)?;
        }
        if let Some((_, writer)) = values.as_mut() {
            write_values_header(writer, params)?;
            let theta: Vec<f64> = params.iter().map(|p| p.value).collect();
            write_values_row(writer, 0, &theta)?;
        }
        Ok(Self { stats, values })
    }

    fn commit(&mut self) -> Result<()> {
        for (path, writer) in [self.stats.as_mut(), self.values.as_mut()].into_iter().flatten() {
            writer.flush()?;
            std::fs::rename(path.with_extension("csv.tmp"), &*path)
                .with_context(|| format!("failed to replace {}", path.display()))?;
        }
        Ok(())
    }

    fn write(&mut self, row: &AppliedRow) -> Result<()> {
        if let Some((_, writer)) = self.stats.as_mut() {
            write_stats_row(writer, row)?;
            writer.flush()?;
        }
        if let Some((_, writer)) = self.values.as_mut() {
            write_values_row(writer, row.completed_pairs, &row.theta)?;
            writer.flush()?;
        }
        Ok(())
    }
}

// =============================================================================
// coordinator
// =============================================================================

/// `--dispatch async` で受け付けない batch 専用の指定を、run-dir に触る前に拒否する。
pub(super) fn validate_cli(cli: &Cli) -> Result<()> {
    let batch_only = [
        ("--batch-pairs", cli.batch_pairs.is_some()),
        ("--games-per-iteration", cli.games_per_iteration.is_some()),
        ("--iterations", cli.iterations.is_some()),
        (
            "--early-stop-avg-abs-update-threshold",
            cli.early_stop_avg_abs_update_threshold.is_some(),
        ),
        (
            "--early-stop-result-variance-threshold",
            cli.early_stop_result_variance_threshold.is_some(),
        ),
        ("--early-stop-patience", cli.early_stop_patience.is_some()),
        ("--force-schedule", cli.force_schedule),
        ("--strict-init-check", cli.strict_init_check),
        ("--meta-file", cli.meta_file.is_some()),
        ("--stats-csv", cli.stats_csv.is_some()),
        ("--param-values-csv", cli.param_values_csv.is_some()),
    ];
    for (flag, given) in batch_only {
        if given {
            bail!("{flag} は --dispatch async では使えません");
        }
    }
    if cli.total_pairs.is_none() {
        bail!("--dispatch async では --total-pairs を指定してください");
    }
    for (flag, value) in [
        ("--max-lag", cli.max_lag),
        ("--max-pair-attempts", cli.max_pair_attempts),
        ("--snapshot-interval", cli.snapshot_interval),
    ] {
        if value == Some(0) {
            bail!("{flag} must be >= 1");
        }
    }
    Ok(())
}

/// main の共通の初期化 (lock・init action・params の読み込み) を済ませた入力。
pub(super) struct AsyncSetup<'a> {
    pub(super) cli: &'a Cli,
    pub(super) action: &'a NonBailAction,
    pub(super) params: Vec<SpsaParam>,
    pub(super) translator: &'a EngineNameTranslator,
    pub(super) active_only_regex: Option<&'a Regex>,
    pub(super) schedule: ScheduleConfig,
    pub(super) total_pairs: u32,
    pub(super) engine_path: PathBuf,
    /// fresh で使う seed (`--seed` 未指定なら乱数)。resume は meta の値を使う。
    pub(super) fresh_seed: u64,
}

/// journal・snapshot・meta・CSV への書き出しをまとめて持つ。
struct Store<'a> {
    cli: &'a Cli,
    meta: AsyncMeta,
    definitions: Vec<ParamDefinition>,
    journal: Journal,
    csv: CsvOutputs,
    generation: u64,
    applied_since_snapshot: u32,
    snapshot_interval: u32,
    faults: &'a FaultPlan,
}

impl Store<'_> {
    fn snapshot_dir(&self) -> PathBuf {
        self.cli.run_dir.join(SNAPSHOT_DIR)
    }

    /// core の出力を順に永続化する。snapshot は出力をすべて書いた後にだけ取る
    /// (θ は複数の適用をまとめて済ませているため、途中の journal 位置と対応しない)。
    fn emit(&mut self, core: &AsyncCore<'_>, outputs: Vec<CoreOutput>) -> Result<()> {
        for output in outputs {
            match output {
                CoreOutput::Journal(record) => {
                    self.journal.append(&record)?;
                    match record {
                        JournalRecord::Received { .. } => self.faults.hit("after_received"),
                        JournalRecord::Accept { .. } => self.faults.hit("after_accept"),
                        JournalRecord::AttemptFailed { .. } => {
                            self.faults.hit("after_attempt_failed")
                        }
                        JournalRecord::Dispatch(_) | JournalRecord::Duplicate { .. } => {}
                    }
                }
                CoreOutput::Applied(row) => {
                    self.csv.write(&row)?;
                    self.applied_since_snapshot += 1;
                }
            }
        }
        if self.applied_since_snapshot >= self.snapshot_interval {
            self.publish(core, RunStatus::Running, None)?;
        }
        Ok(())
    }

    fn publish(
        &mut self,
        core: &AsyncCore<'_>,
        status: RunStatus,
        stop_reason: Option<String>,
    ) -> Result<()> {
        let generation = if self.meta.snapshot.is_empty() {
            0
        } else {
            self.generation + 1
        };
        let snapshot = Snapshot {
            generation,
            journal_offset: self.journal.offset,
            checkpoint: core.checkpoint(),
            param_names: core.params.iter().map(|p| p.name.clone()).collect(),
            theta: core.params.iter().map(|p| p.value).collect(),
            definitions: (generation == 0).then(|| self.definitions.clone()),
            comments: (generation == 0)
                .then(|| core.params.iter().map(|p| p.comment.clone()).collect()),
        };
        let name = snapshot_file_name(generation);
        self.faults.fail("snapshot_io")?;
        write_json_durable(&self.snapshot_dir().join(&name), &snapshot, false)?;
        self.faults.hit("snapshot_written");
        self.faults.fail("meta_io")?;
        self.meta.snapshot = name;
        self.meta.completed_pairs = core.completed;
        self.meta.status = status;
        self.meta.stop_reason = stop_reason;
        self.meta.dispatched_attempts = core.counters.dispatched_attempts;
        self.meta.failed_attempts = core.counters.failures;
        self.meta.duplicates = core.counters.duplicates;
        self.meta.holds = core.counters.holds;
        self.meta.max_observed_lag = core.counters.max_lag;
        self.meta.updated_at_utc = Utc::now().to_rfc3339();
        write_json_durable(&default_meta_path(&self.cli.run_dir), &self.meta, true)?;
        self.faults.hit("meta_written");
        write_params(&state_params_path(&self.cli.run_dir), &core.params)?;
        self.generation = generation;
        self.applied_since_snapshot = 0;
        self.remove_old_snapshots()
    }

    /// gen0 (CSV の再生の起点) と最新 2 世代だけを残す。
    fn remove_old_snapshots(&self) -> Result<()> {
        let keep = [
            snapshot_file_name(0),
            snapshot_file_name(self.generation),
            snapshot_file_name(self.generation.saturating_sub(1)),
        ];
        for entry in std::fs::read_dir(self.snapshot_dir())? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("snapshot-") && !keep.contains(&name) {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    /// 停止前に保留中の結果を含む snapshot を残し、理由を Err で返す。
    fn stop(&mut self, core: &AsyncCore<'_>, error: anyhow::Error) -> Result<()> {
        print_counters("async stopped", core);
        let reason = format!("{error:#}");
        if let Err(save_error) = self.publish(core, RunStatus::Stopped, Some(reason)) {
            eprintln!("failed to save stop snapshot: {save_error:#}");
        }
        Err(error)
    }
}

fn remove_async_artifacts(run_dir: &Path) -> Result<()> {
    let journal = run_dir.join(JOURNAL_FILE);
    if journal.exists() {
        std::fs::remove_file(&journal)
            .with_context(|| format!("failed to remove {}", journal.display()))?;
    }
    let snapshots = run_dir.join(SNAPSHOT_DIR);
    if snapshots.exists() {
        std::fs::remove_dir_all(&snapshots)
            .with_context(|| format!("failed to remove {}", snapshots.display()))?;
    }
    Ok(())
}

fn load_async_meta(path: &Path) -> Result<AsyncMeta> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to open {}", path.display()))?;
    let probe: MetaFormatVersionOnly = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse JSON {}", path.display()))?;
    if probe.format_version != ASYNC_META_FORMAT_VERSION {
        bail!(
            "{} は meta v{} の batch run です。v{} 以前の run は batch 専用として扱い、\
             --dispatch async では再開しません (--dispatch batch で再開してください)",
            path.display(),
            probe.format_version,
            probe.format_version
        );
    }
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse JSON {}", path.display()))
}

fn check_resume_meta(
    meta: &AsyncMeta,
    cli: &Cli,
    total_pairs: u32,
    schedule: ScheduleConfig,
    fingerprint: &RunFingerprint,
    param_definition_sha: &str,
) -> Result<()> {
    if meta.dispatch != "async" || meta.rng_version != RNG_VERSION {
        bail!("unsupported async meta (dispatch={}, rng={})", meta.dispatch, meta.rng_version);
    }
    // 再生は bit 一致を要求するので、batch の許容誤差付き比較ではなく bit で比べる。
    let exact = |a: f64, b: f64| a.to_bits() == b.to_bits();
    let same_schedule = exact(meta.schedule.alpha, schedule.alpha)
        && exact(meta.schedule.gamma, schedule.gamma)
        && exact(meta.schedule.a_ratio, schedule.a_ratio)
        && exact(meta.schedule.mobility, schedule.mobility)
        && meta.schedule.total_iterations == schedule.total_iterations;
    if meta.total_pairs != total_pairs || !same_schedule {
        bail!(
            "total_pairs/schedule が meta と異なります (meta total_pairs={} {:?} / cli total_pairs={total_pairs} {schedule:?})。\
             async では変更できないので新しい run-dir で始めてください",
            meta.total_pairs,
            meta.schedule
        );
    }
    if let Some(seed) = cli.seed
        && seed != meta.base_seed
    {
        bail!("--seed {seed} が meta の base_seed {} と異なります", meta.base_seed);
    }
    if let Some(lag) = cli.max_lag
        && lag != meta.max_lag
    {
        bail!(
            "--max-lag {lag} が meta の {} と異なります (L は fresh で確定します)",
            meta.max_lag
        );
    }
    if meta.param_definition_sha256 != param_definition_sha {
        bail!(
            "param の定義 (順序・型・範囲・c_end/r_end・active) が meta と異なります。新しい run-dir で始めてください"
        );
    }
    if &meta.fingerprint != fingerprint {
        bail!(
            "対局条件が meta と異なります。\n  meta: {:?}\n  cli:  {fingerprint:?}",
            meta.fingerprint
        );
    }
    Ok(())
}

/// async の resume で使う param を gen0 の定義と最新 snapshot の θ から作る。
/// `state.params` は 6 桁表示の view で、丸めた値が範囲外に見えることもあるので読まない。
pub(super) fn load_resume_params(cli: &Cli) -> Result<Vec<SpsaParam>> {
    let meta = load_async_meta(&default_meta_path(&cli.run_dir))?;
    let snapshot_dir = cli.run_dir.join(SNAPSHOT_DIR);
    let initial: Snapshot = read_json(&snapshot_dir.join(snapshot_file_name(0)))?;
    let latest: Snapshot = read_json(&snapshot_dir.join(&meta.snapshot))?;
    let (Some(definitions), Some(comments)) = (initial.definitions, initial.comments) else {
        bail!("gen0 snapshot に param 定義がありません");
    };
    let parse = |text: &str| text.parse::<f64>().with_context(|| format!("invalid number {text}"));
    let mut params = Vec::with_capacity(definitions.len());
    for ((definition, comment), &value) in definitions.iter().zip(comments).zip(&latest.theta) {
        params.push(SpsaParam {
            name: definition.name.clone(),
            type_name: definition.type_name.clone(),
            is_int: definition.type_name.eq_ignore_ascii_case("int"),
            value,
            min: parse(&definition.min)?,
            max: parse(&definition.max)?,
            c_end: parse(&definition.c_end)?,
            r_end: parse(&definition.r_end)?,
            comment,
            not_used: definition.not_used,
        });
    }
    if params.len() != latest.theta.len() {
        bail!("gen0 の param 定義と snapshot {} の θ の長さが一致しません", latest.generation);
    }
    Ok(params)
}

fn apply_theta(params: &mut [SpsaParam], snapshot: &Snapshot) -> Result<()> {
    if snapshot.theta.len() != params.len()
        || params.iter().zip(&snapshot.param_names).any(|(p, name)| p.name != *name)
    {
        bail!("snapshot {} の param 名が state.params と一致しません", snapshot.generation);
    }
    for (p, &value) in params.iter_mut().zip(&snapshot.theta) {
        p.value = value;
    }
    Ok(())
}

fn matches_snapshot(core: &AsyncCore<'_>, snapshot: &Snapshot) -> bool {
    core.checkpoint() == snapshot.checkpoint
        && core
            .params
            .iter()
            .zip(&snapshot.theta)
            .all(|(p, value)| p.value.to_bits() == value.to_bits())
}

pub(super) fn run(setup: AsyncSetup<'_>) -> Result<()> {
    let AsyncSetup {
        cli,
        action,
        params,
        translator,
        active_only_regex,
        schedule,
        total_pairs,
        engine_path,
        fresh_seed,
    } = setup;
    let run_dir = cli.run_dir.as_path();
    let state_params = state_params_path(run_dir);
    let meta_path = default_meta_path(run_dir);
    let journal_path = run_dir.join(JOURNAL_FILE);
    let snapshot_dir = run_dir.join(SNAPSHOT_DIR);
    let faults = FaultPlan::from_env()?;
    let resume = matches!(action, NonBailAction::Resume { .. });

    let start_positions = load_run_start_positions(cli)?;
    let masks = compute_param_masks(&params, active_only_regex, translator, cli)?;
    let param_schedules = compute_param_schedules(&params, schedule);
    // 1 回の更新の勝敗和は 1 pair 分 (最大 2)。
    validate_schedule_bounds(schedule, &params, &masks.active, &param_schedules, 1)?;
    let base_cfg = build_engine_config(cli, engine_path.clone());
    let fingerprint = RunFingerprint::new(cli, &base_cfg)?;
    let definitions = param_definitions(&params, &masks);
    let param_definition_sha = param_definition_sha256(&definitions)?;
    let coordinator_sha256 = std::env::current_exe()
        .context("failed to locate the spsa binary")
        .and_then(|exe| sha256_hex_of_file(&exe))?;

    let meta = if resume {
        let meta = load_async_meta(&meta_path)?;
        check_resume_meta(&meta, cli, total_pairs, schedule, &fingerprint, &param_definition_sha)?;
        if meta.coordinator_sha256 != coordinator_sha256 {
            bail!(
                "spsa binary が meta の記録と異なります (再生の bit 一致と乱数列は同じ binary を前提にしています)。\n  \
                 meta: {}\n  now:  {coordinator_sha256}",
                meta.coordinator_sha256
            );
        }
        meta
    } else {
        let max_lag = match cli.max_lag {
            Some(lag) => lag,
            None => u32::try_from(cli.concurrency)
                .ok()
                .and_then(|w| w.checked_mul(4))
                .context("--max-lag overflow (4 × --concurrency)")?,
        };
        let init = InitMetaSnapshot::for_fresh_start(
            action,
            &state_params,
            cli.init_from.as_deref(),
            &engine_path,
            cli.engine_param_mapping.as_deref(),
        )?;
        AsyncMeta {
            format_version: ASYNC_META_FORMAT_VERSION,
            dispatch: "async".into(),
            rng_version: RNG_VERSION.into(),
            coordinator_sha256,
            base_seed: fresh_seed,
            total_pairs,
            schedule,
            max_lag,
            param_definition_sha256: param_definition_sha.clone(),
            active_param_count: masks.active_count,
            fingerprint: fingerprint.clone(),
            init_params_sha256: init.init_params_sha256,
            init_from_sha256: init.init_from_sha256,
            init_from_path: init.init_from_path,
            engine_path: init.engine_path,
            engine_param_mapping_path: init.engine_param_mapping_path,
            engine_param_mapping_sha256: init.engine_param_mapping_sha256,
            init_mode: init.init_mode,
            snapshot: String::new(),
            completed_pairs: 0,
            status: RunStatus::Running,
            stop_reason: None,
            dispatched_attempts: 0,
            failed_attempts: 0,
            duplicates: 0,
            holds: 0,
            max_observed_lag: 0,
            updated_at_utc: Utc::now().to_rfc3339(),
        }
    };
    eprintln!("using base seed: {}", meta.base_seed);

    let env = CoreEnv {
        schedule,
        big_a: schedule.a_ratio * f64::from(total_pairs),
        param_schedules: &param_schedules,
        active_mask: &masks.active,
        active_only_regex,
        translator,
        start_positions_len: start_positions.len(),
        random_startpos: cli.random_startpos,
        base_seed: meta.base_seed,
        total_pairs,
        max_lag: meta.max_lag,
        max_pair_attempts: cli.max_pair_attempts.unwrap_or(3),
    };
    let mut core = AsyncCore::new(env, params.clone());
    let snapshot_interval = cli.snapshot_interval.unwrap_or(64);

    let mut store = if resume {
        let latest: Snapshot = read_json(&snapshot_dir.join(&meta.snapshot))?;
        let initial: Snapshot = read_json(&snapshot_dir.join(snapshot_file_name(0)))?;
        if initial.definitions.as_ref() != Some(&definitions) {
            bail!(
                "gen0 の param 定義と、CLI (--active-only-regex・mapping) から決まる対象が一致しません"
            );
        }
        apply_theta(&mut core.params, &initial)?;
        // CSV は gen0 からの全再生で作り直す。
        let mut csv = CsvOutputs::begin(cli, &core.params)?;
        let mut verified = false;
        let mut verify = |core: &AsyncCore<'_>| -> Result<()> {
            if !matches_snapshot(core, &latest) {
                bail!(
                    "journal の再生結果が snapshot {} と一致しません (同じ binary・環境での再生を前提にしています)",
                    latest.generation
                );
            }
            verified = true;
            Ok(())
        };
        let valid_len = read_journal(&journal_path, |offset, record| {
            if offset == latest.journal_offset {
                verify(&core)?;
            }
            if let Some(row) = core.replay(record)? {
                csv.write(&row)?;
                faults.hit("csv_rebuild");
            }
            Ok(())
        })?;
        if valid_len == latest.journal_offset {
            verify(&core)?;
        }
        if !verified {
            bail!(
                "snapshot {} の journal 位置 {} が journal (有効長 {valid_len}) の record 境界にありません",
                latest.generation,
                latest.journal_offset
            );
        }
        csv.commit()?;
        let mut store = Store {
            cli,
            meta,
            definitions,
            journal: Journal::open_at(&journal_path, valid_len)?,
            csv,
            generation: latest.generation,
            applied_since_snapshot: 0,
            snapshot_interval,
            faults: &faults,
        };
        let outputs = core.finish_replay()?;
        store.emit(&core, outputs)?;
        store
    } else {
        remove_async_artifacts(run_dir)?;
        remove_stale_final_params_for_fresh_start(run_dir)?;
        std::fs::create_dir_all(&snapshot_dir)
            .with_context(|| format!("failed to create {}", snapshot_dir.display()))?;
        sync_dir(run_dir)?;
        let mut csv = CsvOutputs::begin(cli, &core.params)?;
        csv.commit()?;
        let mut store = Store {
            cli,
            meta,
            definitions,
            journal: Journal::create(&journal_path)?,
            csv,
            generation: 0,
            applied_since_snapshot: 0,
            snapshot_interval,
            faults: &faults,
        };
        store.publish(&core, RunStatus::Running, None)?;
        store
    };

    let remaining = total_pairs - core.completed;
    let worker_count = cli.concurrency.min(remaining as usize);
    eprintln!("=== SPSA Async Startup ===");
    eprintln!("launch action:  {}", action.launch_label());
    eprintln!("origin mode:    {}", store.meta.init_mode);
    eprintln!("params:         {}", state_params.display());
    eprintln!("journal:        {}", journal_path.display());
    eprintln!(
        "pairs:          completed {}/{total_pairs}, pending {}, held {}, workers {worker_count}",
        core.completed,
        core.pairs.len(),
        core.held.len()
    );
    eprintln!(
        "policy:         max_lag={} max_pair_attempts={} snapshot_interval={snapshot_interval} engine_retries={}",
        core.env.max_lag, core.env.max_pair_attempts, cli.engine_retries
    );
    eprintln!("active params:  {}/{}", masks.active_count, params.len());
    eprintln!("=== End Summary ===");

    if !core.is_complete() {
        let cpu_affinities = plan_cpu_affinities(cli, worker_count)?;
        let (game_cfg, tc) = build_game_setup(cli);
        std::thread::scope(|scope| -> Result<()> {
            let pool = AsyncPool::new(
                scope,
                worker_count,
                WorkerContext {
                    base_cfg: &base_cfg,
                    params: &params,
                    start_positions: &start_positions,
                    game_cfg: &game_cfg,
                    tc,
                    translator,
                    application_mask: &masks.application,
                    cpu_affinities: cpu_affinities.as_deref(),
                },
                cli.engine_retries,
                &faults,
            )?;
            coordinate(&mut core, &mut store, &pool, worker_count)
        })?;
    }

    store.publish(&core, RunStatus::Completed, None)?;
    faults.hit("completed_snapshot");
    print_counters("async done", &core);
    let final_path = run_dir.join("final.params");
    write_params(&final_path, &core.params)?;
    eprintln!("final params written: {}", final_path.display());
    Ok(())
}

fn print_counters(label: &str, core: &AsyncCore<'_>) {
    let counters = core.counters;
    eprintln!(
        "{label}: completed_pairs={} dispatched_attempts={} failed_attempts={} duplicates={} holds={} max_lag={}",
        core.completed,
        counters.dispatched_attempts,
        counters.failures,
        counters.duplicates,
        counters.holds,
        counters.max_lag
    );
}

/// 空いた worker に pair を渡し、結果を到着順に処理する。停止は snapshot を残して Err。
fn coordinate(
    core: &mut AsyncCore<'_>,
    store: &mut Store<'_>,
    pool: &AsyncPool<'_>,
    worker_count: usize,
) -> Result<()> {
    let mut in_flight = HashSet::new();
    while !core.is_complete() {
        // 順序: 保留の適用 (on_done・finish_replay 内) → 再 attempt を含む dispatch → 停滞の判定。
        if let Some(reason) = core.exhausted_retry() {
            return store.stop(core, anyhow::anyhow!(reason));
        }
        while in_flight.len() < worker_count {
            let Some(dispatch) = core.next_dispatch()? else {
                break;
            };
            store.journal.append(&JournalRecord::Dispatch(dispatch.clone()))?;
            store.faults.hit("after_dispatch");
            let (plus, minus) = core.engine_values(&dispatch);
            in_flight.insert((dispatch.pair_id, dispatch.attempt));
            pool.send(PairTask {
                pair_id: dispatch.pair_id,
                attempt: dispatch.attempt,
                start_pos_index: dispatch.start_pos_index,
                plus,
                minus,
            })?;
        }
        let stalled = if in_flight.len() < worker_count {
            core.stall_reason()
        } else {
            None
        };
        // 停滞と判定しても、受信済みで未処理の結果があればそれを先に処理する。
        let message = match (stalled, pool.try_recv()) {
            (_, Some(message)) => message,
            (Some(reason), None) => return store.stop(core, anyhow::anyhow!(reason)),
            (None, None) if in_flight.is_empty() => {
                // 最古の未確定 pair は常に適用か dispatch ができるので、ここへは来ない。
                return store.stop(core, anyhow::anyhow!("no runnable pair"));
            }
            (None, None) => match pool.recv() {
                Ok(message) => message,
                Err(error) => return store.stop(core, error),
            },
        };
        let message = match message {
            Ok(message) => message,
            Err(error) => return store.stop(core, error),
        };
        match message {
            WorkerMessage::Done {
                pair_id,
                attempt,
                games,
            } => {
                in_flight.remove(&(pair_id, attempt));
                let outputs = core.on_done(pair_id, attempt, games)?;
                store.emit(core, outputs)?;
            }
            WorkerMessage::Failed {
                pair_id,
                attempt,
                reason,
            } => {
                in_flight.remove(&(pair_id, attempt));
                eprintln!("attempt failed pair={pair_id} attempt={attempt} cause={reason}");
                let (output, stop) = core.on_failed(pair_id, attempt, reason);
                store.emit(core, vec![output])?;
                if let Some(reason) = stop {
                    return store.stop(core, anyhow::anyhow!(reason));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    use super::*;

    fn param(name: &str, is_int: bool, value: f64, range: (f64, f64), c_end: f64) -> SpsaParam {
        SpsaParam {
            name: name.into(),
            type_name: if is_int { "int" } else { "float" }.into(),
            is_int,
            value,
            min: range.0,
            max: range.1,
            c_end,
            r_end: 0.002,
            comment: String::new(),
            not_used: false,
        }
    }

    struct Fixture {
        params: Vec<SpsaParam>,
        regex: Option<Regex>,
        schedules: Vec<ParamScheduleConstants>,
        active: Vec<bool>,
        translator: EngineNameTranslator,
        schedule: ScheduleConfig,
    }

    impl Fixture {
        fn new(params: Vec<SpsaParam>, regex: Option<&str>, total_pairs: u32) -> Self {
            let regex = regex.map(|text| Regex::new(text).unwrap());
            let translator = EngineNameTranslator::empty();
            let active =
                params.iter().map(|p| is_param_active(p, regex.as_ref(), &translator)).collect();
            let schedule = ScheduleConfig {
                alpha: 0.602,
                gamma: 0.101,
                a_ratio: 0.1,
                mobility: 1.0,
                total_iterations: total_pairs,
            };
            let schedules = compute_param_schedules(&params, schedule);
            Self {
                params,
                regex,
                schedules,
                active,
                translator,
                schedule,
            }
        }

        fn env(&self, max_lag: u32) -> CoreEnv<'_> {
            CoreEnv {
                schedule: self.schedule,
                big_a: self.schedule.a_ratio * f64::from(self.schedule.total_iterations),
                param_schedules: &self.schedules,
                active_mask: &self.active,
                active_only_regex: self.regex.as_ref(),
                translator: &self.translator,
                start_positions_len: 3,
                random_startpos: true,
                base_seed: 7,
                total_pairs: self.schedule.total_iterations,
                max_lag,
                max_pair_attempts: 3,
            }
        }

        fn core(&self, max_lag: u32) -> AsyncCore<'_> {
            AsyncCore::new(self.env(max_lag), self.params.clone())
        }
    }

    fn game(plus_is_black: bool, plus_score: i8) -> GameRecord {
        GameRecord {
            plus_is_black,
            plus_score,
            reason: "resign".into(),
            plies: 10,
        }
    }

    fn games(first: i8, second: i8) -> [GameRecord; 2] {
        [game(true, first), game(false, second)]
    }

    /// 既知最適点 θ* を持つ quadratic 応答。plus/minus の確定値だけから勝敗を決める。
    struct Quadratic {
        optimum: Vec<f64>,
        curvature: Vec<f64>,
        beta: f64,
        black_bias: f64,
        draw_rate: f64,
        rng: ChaCha8Rng,
    }

    impl Quadratic {
        fn value(&self, theta: &[f64]) -> f64 {
            -0.5 * theta
                .iter()
                .zip(&self.optimum)
                .zip(&self.curvature)
                .map(|((v, o), h)| h * (v - o) * (v - o))
                .sum::<f64>()
        }

        fn play(&mut self, dispatch: &DispatchRecord) -> [GameRecord; 2] {
            let diff = self.value(&dispatch.plus) - self.value(&dispatch.minus);
            let mut play = |plus_is_black: bool| {
                let bias = if plus_is_black {
                    self.black_bias
                } else {
                    -self.black_bias
                };
                let p_plus = 1.0 / (1.0 + (-(self.beta * diff + bias)).exp());
                let score = if self.rng.random::<f64>() < self.draw_rate {
                    0
                } else if self.rng.random::<f64>() < p_plus {
                    1
                } else {
                    -1
                };
                game(plus_is_black, score)
            };
            [play(true), play(false)]
        }
    }

    struct SimResult {
        journal: Vec<JournalRecord>,
        rows: Vec<AppliedRow>,
        stop: Option<String>,
    }

    /// 仮想時刻のヒープで完了順を決める executor。`duration` は (pair, attempt, 結果) から所要時間を返す。
    fn simulate(
        core: &mut AsyncCore<'_>,
        workers: usize,
        mut play: impl FnMut(&DispatchRecord) -> [GameRecord; 2],
        mut duration: impl FnMut(&DispatchRecord, &[GameRecord; 2]) -> u64,
    ) -> SimResult {
        let mut heap = BinaryHeap::new();
        let mut outcomes = HashMap::new();
        let mut now = 0u64;
        let mut seq = 0u64;
        let mut result = SimResult {
            journal: Vec::new(),
            rows: Vec::new(),
            stop: None,
        };
        while !core.is_complete() {
            while heap.len() < workers {
                let Some(dispatch) = core.next_dispatch().unwrap() else {
                    break;
                };
                result.journal.push(JournalRecord::Dispatch(dispatch.clone()));
                let outcome = play(&dispatch);
                let finish = now + duration(&dispatch, &outcome);
                outcomes.insert(seq, outcome);
                heap.push(Reverse((finish, seq, dispatch.pair_id, dispatch.attempt)));
                seq += 1;
            }
            let reserved = core.completed as usize + core.pairs.len();
            assert!(reserved <= core.env.total_pairs as usize, "budget invariant");
            let ks = core.sorted_ks(None);
            assert!(lag_feasible(core.completed, &ks, core.env.max_lag), "lag invariant");
            if heap.len() < workers
                && let Some(reason) = core.stall_reason()
            {
                result.stop = Some(reason);
                return result;
            }
            let Reverse((finish, id, pair_id, attempt)) = heap.pop().expect("no runnable pair");
            let outcome = outcomes.remove(&id).unwrap();
            now = finish;
            for output in core.on_done(pair_id, attempt, outcome).unwrap() {
                match output {
                    CoreOutput::Journal(record) => result.journal.push(record),
                    CoreOutput::Applied(row) => {
                        assert!(row.lag <= core.env.max_lag, "applied lag {}", row.lag);
                        result.rows.push(*row);
                    }
                }
            }
        }
        result
    }

    fn theta_bits(params: &[SpsaParam]) -> Vec<u64> {
        params.iter().map(|p| p.value.to_bits()).collect()
    }

    fn replay_all<'a>(
        fixture: &'a Fixture,
        max_lag: u32,
        journal: &[JournalRecord],
    ) -> AsyncCore<'a> {
        let mut core = fixture.core(max_lag);
        for record in journal {
            core.replay(record).unwrap();
        }
        core
    }

    #[test]
    fn lag_feasibility_counts_tied_oldest_pairs() {
        // W=3・L=12 で k=0 の 2 pair が残ったまま 12 pair が先に確定した状態。
        assert!(lag_feasible(12, &[0, 0], 13));
        assert!(!lag_feasible(12, &[0, 0], 12));
        assert!(lag_feasible(11, &[0, 0], 12));
        assert!(lag_feasible(5, &[], 0));
    }

    #[test]
    fn one_pair_update_uses_dispatch_coefficients_and_pair_score_scale() {
        for score in [1i8, -1] {
            let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 10);
            let mut core = fixture.core(4);
            let dispatch = core.next_dispatch().unwrap().unwrap();
            let flip = if dispatch.flips == "+" { 1.0 } else { -1.0 };
            core.on_done(0, 1, games(score, score)).unwrap();
            let (c_k, r_k) = fixture.schedules[0].at_iteration(0, 1.0, 0.602, 0.101);
            let expected = 50.0 + r_k * c_k * f64::from(2 * score) * flip;
            assert_eq!(core.params[0].value.to_bits(), expected.to_bits());
        }
    }

    #[test]
    fn stale_dispatch_k_coefficients_are_used_when_applied_later() {
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 10);
        let mut core = fixture.core(4);
        let first = core.next_dispatch().unwrap().unwrap();
        let second = core.next_dispatch().unwrap().unwrap();
        assert_eq!((first.dispatch_k, second.dispatch_k), (0, 0));
        core.on_done(0, 1, games(1, 1)).unwrap();
        let before = core.params[0].value;
        let outputs = core.on_done(1, 1, games(-1, -1)).unwrap();
        let CoreOutput::Applied(row) = &outputs[2] else {
            panic!("expected applied row");
        };
        assert_eq!((row.dispatch_k, row.lag), (0, 1));
        let flip = if second.flips == "+" { 1.0 } else { -1.0 };
        let (c_k, r_k) = fixture.schedules[0].at_iteration(0, 1.0, 0.602, 0.101);
        let expected = (before - 2.0 * r_k * c_k * flip).clamp(0.0, 100.0);
        assert_eq!(core.params[0].value.to_bits(), expected.to_bits());
    }

    #[test]
    fn clamp_bounds_and_integer_theta_keeps_fraction() {
        let mut p = param("I", true, 9.6, (0.0, 10.0), 1.0);
        p.r_end = 0.5;
        let fixture = Fixture::new(vec![p], None, 4);
        let mut core = fixture.core(4);
        let dispatch = core.next_dispatch().unwrap().unwrap();
        assert!(dispatch.plus[0].fract() == 0.0 && dispatch.minus[0].fract() == 0.0);
        let score = if dispatch.flips == "+" { 1 } else { -1 };
        core.on_done(0, 1, games(score, score)).unwrap();
        assert_eq!(core.params[0].value, 10.0, "clamped at max");
        let dispatch = core.next_dispatch().unwrap().unwrap();
        let score = if dispatch.flips == "+" { -1 } else { 1 };
        core.on_done(1, 1, games(score, 0)).unwrap();
        assert!(core.params[0].value < 10.0 && core.params[0].value.fract() != 0.0);
    }

    #[test]
    fn single_worker_matches_batch_pairs_one_bit_for_bit() {
        let params = vec![
            param("A", true, 20.0, (0.0, 40.0), 3.0),
            param("B", false, 1.5, (0.0, 3.0), 0.4),
            param("FIXED", true, 7.4, (0.0, 10.0), 1.0),
        ];
        let total = 40;
        let fixture = Fixture::new(params.clone(), Some("^(A|B)$"), total);
        // 決定的な応答: plus が最適点に近ければ 2 局とも勝ち。
        let respond = |plus: &[f64], minus: &[f64]| -> i8 {
            let loss = |v: &[f64]| (v[0] - 26.0).powi(2) + 30.0 * (v[1] - 2.2).powi(2);
            match loss(plus).partial_cmp(&loss(minus)).unwrap() {
                std::cmp::Ordering::Less => 1,
                std::cmp::Ordering::Greater => -1,
                std::cmp::Ordering::Equal => 0,
            }
        };
        let mut core = fixture.core(4);
        let result = simulate(
            &mut core,
            1,
            |d| {
                let score = respond(&d.plus, &d.minus);
                games(score, score)
            },
            |_, _| 1,
        );
        assert!(result.stop.is_none());

        let mut batch = params;
        for k in 0..total {
            let ctx = BatchPrepCtx {
                big_a: fixture.schedule.a_ratio * f64::from(total),
                schedule: fixture.schedule,
                params: &batch,
                param_schedules: &fixture.schedules,
                active_only_regex: fixture.regex.as_ref(),
                translator: &fixture.translator,
                start_positions_len: 3,
                batch_pairs: 1,
                random_startpos: true,
            };
            let prep = compute_batch_prep(&ctx, k, k, 7, 2 * k as usize).unwrap();
            let score = respond(&prep.plus_values[..2], &prep.minus_values[..2]);
            assert_eq!(prep.plus_values[2], 7.0);
            apply_spsa_update(
                &mut batch,
                &prep.flips,
                &fixture.schedules,
                &fixture.active,
                fixture.schedule,
                k,
                f64::from(2 * score),
            );
            let row = &result.rows[k as usize];
            assert_eq!(
                row.theta.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                theta_bits(&batch),
                "k={k}"
            );
        }
        assert_eq!(theta_bits(&core.params), theta_bits(&batch));
        let dispatch = core.engine_values(&DispatchRecord {
            pair_id: 0,
            attempt: 1,
            dispatch_k: 0,
            start_pos_index: 0,
            flips: "++".into(),
            plus: vec![1.0, 2.0],
            minus: vec![3.0, 4.0],
        });
        assert_eq!(dispatch, (vec![1.0, 2.0, 7.0], vec![3.0, 4.0, 7.0]));
    }

    #[test]
    fn tied_oldest_pairs_never_exceed_lag_bound() {
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 60);
        let mut core = fixture.core(12);
        // pair 0 と 1 (どちらも k=0) だけが遅い。
        let result =
            simulate(&mut core, 3, |_| games(1, 0), |d, _| if d.pair_id < 2 { 15 } else { 1 });
        assert!(result.stop.is_none());
        assert!(core.is_complete());
        assert!(result.rows.iter().all(|row| row.lag <= 12));
        assert_eq!(result.rows.iter().map(|row| row.lag).max(), Some(12));
    }

    #[test]
    fn oldest_result_is_applied_before_held_younger_results() {
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 10);
        let mut core = fixture.core(1);
        let _a = core.next_dispatch().unwrap().unwrap();
        let _b = core.next_dispatch().unwrap().unwrap();
        assert!(core.next_dispatch().unwrap().is_none(), "C + 2 - 0 > L");
        // B (k=0) は A と同じ k なので先に適用できる。続く C (k=1) は A を lag 2 にするので保留。
        assert_eq!(core.on_done(1, 1, games(1, 1)).unwrap().len(), 3);
        let c = core.next_dispatch().unwrap().unwrap();
        assert_eq!((c.pair_id, c.dispatch_k), (2, 1));
        assert!(core.next_dispatch().unwrap().is_none());
        let outputs = core.on_done(2, 1, games(1, 1)).unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(core.held, vec![2]);
        let outputs = core.on_done(0, 1, games(-1, -1)).unwrap();
        let accepted: Vec<u32> = outputs
            .iter()
            .filter_map(|output| match output {
                CoreOutput::Journal(JournalRecord::Accept { pair_id }) => Some(*pair_id),
                _ => None,
            })
            .collect();
        assert_eq!(accepted, vec![0, 2]);
        assert_eq!(core.counters.holds, 1);
        assert_eq!(core.counters.max_lag, 1);
    }

    #[test]
    fn stalled_oldest_stops_and_resume_does_not_stop_immediately() {
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 100);
        let mut core = fixture.core(8);
        let result = simulate(
            &mut core,
            4,
            |_| games(1, -1),
            |d, _| if d.pair_id == 0 { u64::MAX / 2 } else { 1 },
        );
        let reason = result.stop.expect("stall must stop");
        assert!(reason.contains("pair 0"), "{reason}");
        assert_eq!(core.completed, 8);
        let held = core.held.len();
        assert!(held > 0);

        let mut resumed = replay_all(&fixture, 8, &result.journal);
        assert_eq!(resumed.checkpoint(), core.checkpoint());
        let outputs = resumed.finish_replay().unwrap();
        assert!(outputs.is_empty());
        let retry = resumed.next_dispatch().unwrap().unwrap();
        assert_eq!((retry.pair_id, retry.attempt), (0, 2));
        assert!(resumed.stall_reason().is_none());
        // 出し直した最古が届けば保留がすべて適用される。
        let outputs = resumed.on_done(0, 2, games(1, 1)).unwrap();
        assert!(resumed.held.is_empty());
        assert_eq!(
            outputs.iter().filter(|o| matches!(o, CoreOutput::Applied(_))).count(),
            held + 1
        );
    }

    #[test]
    fn slow_pair_at_budget_end_waits_instead_of_stopping() {
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 12);
        let mut core = fixture.core(2);
        let result = simulate(
            &mut core,
            2,
            |_| games(1, -1),
            |d, _| if d.pair_id == 10 { 1_000 } else { 1 },
        );
        assert!(result.stop.is_none(), "{:?}", result.stop);
        assert!(core.is_complete());
    }

    #[test]
    fn failed_attempt_retries_same_values_until_failure_limit() {
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 5);
        let mut core = fixture.core(4);
        let first = core.next_dispatch().unwrap().unwrap();
        for attempt in 1..=3 {
            let (_, stop) = core.on_failed(0, attempt, "engine exited".into());
            let retry = core.next_dispatch().unwrap().unwrap();
            assert_eq!(retry.attempt, attempt + 1);
            assert_eq!(
                (retry.plus.clone(), retry.minus.clone(), retry.start_pos_index, retry.dispatch_k),
                (first.plus.clone(), first.minus.clone(), first.start_pos_index, first.dispatch_k)
            );
            assert_eq!(stop.is_some(), attempt == 3);
        }
        assert_eq!(core.counters.failures, 3);
        // 古い attempt の失敗通知は数えない。
        let (_, stop) = core.on_failed(0, 1, "late".into());
        assert!(stop.is_none());
        assert_eq!(core.counters.failures, 3);
    }

    #[test]
    fn duplicate_results_are_recorded_and_applied_once() {
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 5);
        let mut core = fixture.core(1);
        core.next_dispatch().unwrap();
        core.next_dispatch().unwrap();
        core.on_done(1, 1, games(1, 1)).unwrap();
        let held_dup = core.on_done(1, 1, games(1, 1)).unwrap();
        assert!(matches!(
            held_dup[..],
            [CoreOutput::Journal(JournalRecord::Duplicate {
                pair_id: 1,
                attempt: 1
            })]
        ));
        core.on_done(0, 1, games(1, 1)).unwrap();
        let theta = core.params[0].value;
        let accepted_dup = core.on_done(0, 1, games(-1, -1)).unwrap();
        assert!(matches!(
            accepted_dup[..],
            [CoreOutput::Journal(JournalRecord::Duplicate {
                pair_id: 0,
                ..
            })]
        ));
        assert_eq!(core.params[0].value, theta);
        assert_eq!((core.completed, core.counters.duplicates), (2, 2));
        assert!(core.on_done(9, 1, games(1, 1)).is_err());
    }

    #[test]
    fn fewer_pairs_than_workers_dispatches_only_budget() {
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 2);
        let mut core = fixture.core(20);
        let result = simulate(&mut core, 5, |_| games(1, 1), |_, _| 1);
        let dispatches = result
            .journal
            .iter()
            .filter(|r| matches!(r, JournalRecord::Dispatch(_)))
            .count();
        assert_eq!(dispatches, 2);
        assert!(core.is_complete());
    }

    #[test]
    fn replayed_journal_reproduces_live_state_bit_for_bit() {
        let params = vec![
            param("A", true, 20.0, (0.0, 40.0), 3.0),
            param("B", false, 1.5, (0.0, 3.0), 0.4),
        ];
        let fixture = Fixture::new(params, None, 80);
        let mut core = fixture.core(16);
        let mut model = Quadratic {
            optimum: vec![26.0, 2.2],
            curvature: vec![0.02, 3.0],
            beta: 2.0,
            black_bias: 0.2,
            draw_rate: 0.2,
            rng: ChaCha8Rng::seed_from_u64(3),
        };
        // 勝敗と相関した長い裾 (plus 勝ちの局は長い) と、pair ごとの揺らぎ。
        let result = simulate(
            &mut core,
            4,
            |d| model.play(d),
            |d, g| {
                let base = 10 + u64::from(d.pair_id * 7919 % 13);
                if g[0].plus_score > 0 { base * 6 } else { base }
            },
        );
        assert!(result.stop.is_none());
        assert!(result.rows.iter().any(|row| row.lag > 0));
        let replayed = replay_all(&fixture, 16, &result.journal);
        assert_eq!(theta_bits(&replayed.params), theta_bits(&core.params));
        assert_eq!(replayed.checkpoint(), core.checkpoint());
        let applied = result
            .journal
            .iter()
            .filter(|r| matches!(r, JournalRecord::Accept { .. }))
            .count();
        assert_eq!(applied, 80);
        // JSON 往復しても同じ再生結果になる。
        let lines: Vec<String> =
            result.journal.iter().map(|r| serde_json::to_string(r).unwrap()).collect();
        let parsed: Vec<JournalRecord> =
            lines.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(parsed, result.journal);
    }

    #[test]
    fn resume_drains_held_results_received_before_crash() {
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 6);
        let mut core = fixture.core(1);
        let mut journal = Vec::new();
        let record = |outputs: Vec<CoreOutput>, journal: &mut Vec<JournalRecord>| {
            for output in outputs {
                if let CoreOutput::Journal(record) = output {
                    journal.push(record);
                }
            }
        };
        for _ in 0..2 {
            journal.push(JournalRecord::Dispatch(core.next_dispatch().unwrap().unwrap()));
        }
        record(core.on_done(1, 1, games(1, 1)).unwrap(), &mut journal);
        journal.push(JournalRecord::Dispatch(core.next_dispatch().unwrap().unwrap()));
        // pair 2 の受信 (保留) を記録した直後、pair 0 の結果を受ける前に落ちた。
        record(core.on_done(2, 1, games(1, 1)).unwrap(), &mut journal);
        assert_eq!(core.held, vec![2]);
        let mut resumed = replay_all(&fixture, 1, &journal);
        assert_eq!(resumed.checkpoint(), core.checkpoint());
        assert!(resumed.finish_replay().unwrap().is_empty());
        assert_eq!(resumed.held, vec![2]);
        let retry = resumed.next_dispatch().unwrap().unwrap();
        assert_eq!((retry.pair_id, retry.attempt), (0, 2));
        let outputs = resumed.on_done(0, 2, games(-1, -1)).unwrap();
        assert_eq!(outputs.iter().filter(|o| matches!(o, CoreOutput::Applied(_))).count(), 2);
    }

    #[test]
    fn snapshot_after_resume_drain_matches_later_replay() {
        // A・B を出し、B を受信した直後に落ちる。resume で A は retry_wait になり、B の適用で snapshot を取る。
        // A を出し直す前にまた落ちると、次の再生ではその位置で A は in_flight に見える。
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 4);
        let mut core = fixture.core(1);
        let mut journal = Vec::new();
        for _ in 0..2 {
            journal.push(JournalRecord::Dispatch(core.next_dispatch().unwrap().unwrap()));
        }
        let received = JournalRecord::Received {
            pair_id: 1,
            attempt: 1,
            games: games(1, 1),
        };
        journal.push(received);
        let mut resumed = replay_all(&fixture, 1, &journal);
        for output in resumed.finish_replay().unwrap() {
            if let CoreOutput::Journal(record) = output {
                journal.push(record);
            }
        }
        assert!(matches!(journal.last(), Some(JournalRecord::Accept { pair_id: 1 })));
        let snapshot = resumed.checkpoint();
        let replayed = replay_all(&fixture, 1, &journal);
        assert_eq!(replayed.pairs[&0].status, PairStatus::InFlight);
        assert_eq!(resumed.pairs[&0].status, PairStatus::RetryWait);
        assert_eq!(replayed.checkpoint(), snapshot);
        assert_eq!(theta_bits(&replayed.params), theta_bits(&resumed.params));
    }

    #[test]
    fn invalid_transitions_are_rejected_on_replay() {
        let fixture = Fixture::new(vec![param("X", false, 50.0, (0.0, 100.0), 4.0)], None, 3);
        let mut core = fixture.core(4);
        assert!(core.replay(&JournalRecord::Accept { pair_id: 0 }).is_err());
        let mut core = fixture.core(4);
        let dispatch = core.next_dispatch().unwrap().unwrap();
        let mut replayed = fixture.core(4);
        replayed.replay(&JournalRecord::Dispatch(dispatch.clone())).unwrap();
        assert!(replayed.replay(&JournalRecord::Dispatch(dispatch.clone())).is_err());
        let failed = JournalRecord::AttemptFailed {
            pair_id: 0,
            attempt: 2,
            reason: String::new(),
        };
        assert!(replayed.replay(&failed).is_err());
        assert!(
            replayed
                .replay(&JournalRecord::Duplicate {
                    pair_id: 5,
                    attempt: 1
                })
                .is_err()
        );
        let mut skewed = dispatch;
        skewed.pair_id = 1;
        skewed.dispatch_k = 3;
        assert!(replayed.replay(&JournalRecord::Dispatch(skewed)).is_err());
        assert!(
            replayed
                .replay(&JournalRecord::Received {
                    pair_id: 0,
                    attempt: 2,
                    games: games(1, 1)
                })
                .is_err()
        );
    }

    #[test]
    fn journal_reader_discards_only_unterminated_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let good = serde_json::to_string(&JournalRecord::Accept { pair_id: 0 }).unwrap() + "\n";
        std::fs::write(&path, format!("{good}{{\"kind\":\"acc")).unwrap();
        let mut seen = Vec::new();
        let len = read_journal(&path, |offset, record| {
            seen.push((offset, record.clone()));
            Ok(())
        })
        .unwrap();
        assert_eq!(len, good.len() as u64);
        assert_eq!(seen.len(), 1);
        let journal = Journal::open_at(&path, len).unwrap();
        assert_eq!(journal.offset, len);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), good);

        std::fs::write(&path, format!("{good}{{\"kind\":\"unknown\"}}\n")).unwrap();
        assert!(read_journal(&path, |_, _| Ok(())).is_err());
        std::fs::write(&path, format!("not json\n{good}")).unwrap();
        assert!(read_journal(&path, |_, _| Ok(())).is_err());
    }

    #[test]
    fn exact_f64_strings_round_trip_bits() {
        let values = vec![0.1 + 0.2, 1e-300, -2.5e17, f64::MIN_POSITIVE, 1.0 / 3.0];
        let dispatch = DispatchRecord {
            pair_id: 0,
            attempt: 1,
            dispatch_k: 0,
            start_pos_index: 0,
            flips: "+".into(),
            plus: values.clone(),
            minus: values.clone(),
        };
        let text = serde_json::to_string(&JournalRecord::Dispatch(dispatch)).unwrap();
        let JournalRecord::Dispatch(parsed) = serde_json::from_str(&text).unwrap() else {
            panic!("dispatch expected");
        };
        assert_eq!(
            parsed.plus.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            values.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn quadratic_optimum_distance_shrinks_across_fixed_seeds() {
        let mut improved = 0;
        let seeds = 0..8u64;
        for seed in seeds.clone() {
            let params = vec![
                param("A", true, 10.0, (0.0, 60.0), 2.0),
                param("B", false, 0.5, (0.0, 3.0), 0.15),
                param("C", true, 40.0, (0.0, 60.0), 2.0),
            ];
            let optimum = vec![30.0, 2.0, 25.0];
            let mut fixture = Fixture::new(params, None, 600);
            for p in &mut fixture.params {
                p.r_end = 0.01;
            }
            fixture.schedules = compute_param_schedules(&fixture.params, fixture.schedule);
            let mut env = fixture.env(32);
            env.base_seed = seed;
            let mut core = AsyncCore::new(env, fixture.params.clone());
            let mut model = Quadratic {
                optimum: optimum.clone(),
                curvature: vec![0.01, 2.0, 0.01],
                beta: 1.0,
                black_bias: 0.3,
                draw_rate: 0.1,
                rng: ChaCha8Rng::seed_from_u64(seed + 100),
            };
            let result =
                simulate(&mut core, 8, |d| model.play(d), |d, _| 5 + u64::from(d.pair_id % 7));
            assert!(result.stop.is_none());
            let distance = |values: &[f64]| {
                values
                    .iter()
                    .zip(&optimum)
                    .zip([60.0, 3.0, 60.0])
                    .map(|((v, o), range)| ((v - o) / range).powi(2))
                    .sum::<f64>()
            };
            let start = distance(&fixture.params.iter().map(|p| p.value).collect::<Vec<_>>());
            let end = distance(&core.params.iter().map(|p| p.value).collect::<Vec<_>>());
            if end < start {
                improved += 1;
            }
        }
        assert!(improved >= 7, "improved in {improved}/8 seeds");
    }
}
