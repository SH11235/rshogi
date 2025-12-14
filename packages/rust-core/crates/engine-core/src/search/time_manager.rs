//! 時間管理（TimeManagement）
//!
//! 使用可能な最大時間、対局の手数、その他のパラメータに応じて、
//! 思考に費やす最適な時間を計算する。

use super::{LimitsType, TimeOptions, TimePoint};
use crate::time::Instant;
use crate::types::Color;
use log::debug;
use rand::Rng;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// =============================================================================
// 定数
// =============================================================================

/// デフォルトの最小思考時間（ミリ秒） - YaneuraOu準拠
const DEFAULT_MINIMUM_THINKING_TIME: TimePoint = 2000;

/// デフォルトのネットワーク遅延（ミリ秒）
const DEFAULT_NETWORK_DELAY: TimePoint = 120;

/// デフォルトのネットワーク遅延2（ミリ秒）
const DEFAULT_NETWORK_DELAY2: TimePoint = 1120;

/// デフォルトのSlowMover（百分率）
const DEFAULT_SLOW_MOVER: i32 = 100;

/// MinimumThinkingTime の下限（ミリ秒）
const MIN_MINIMUM_THINKING_TIME: TimePoint = 1000;

/// 引き分けまでの最大手数のデフォルト値
pub const DEFAULT_MAX_MOVES_TO_DRAW: i32 = 100000;

/// 合法手1つの場合の時間上限（ミリ秒）- YaneuraOu準拠
const SINGLE_MOVE_TIME_LIMIT: TimePoint = 500;

/// 最善手不安定性係数の定数 - YaneuraOu準拠
/// bestMoveInstability = BASE + FACTOR * totBestMoveChanges / threads.size()
/// 注: クランプなし（YaneuraOu準拠）
const BEST_MOVE_INSTABILITY_BASE: f64 = 0.9929;
const BEST_MOVE_INSTABILITY_FACTOR: f64 = 1.8519;

// =============================================================================
// 公開関数
// =============================================================================

/// MoveHorizon（残り手数見積もり）を計算（YaneuraOu準拠）
///
/// # Arguments
/// * `time_forfeit` - 切れ負けルールか（inc=0かつbyoyomi=0）
/// * `ply` - 現在の手数
///
/// # Returns
/// 残り手数の見積もり
pub fn calculate_move_horizon(time_forfeit: bool, ply: i32) -> i32 {
    const MOVE_HORIZON: i32 = 160;

    if time_forfeit {
        // 切れ負けルール: MoveHorizon + 40 - min(ply, 40)
        MOVE_HORIZON + 40 - ply.min(40)
    } else {
        // フィッシャールール: MoveHorizon + 20 - min(ply, 80)
        MOVE_HORIZON + 20 - ply.min(80)
    }
}

/// 最善手不安定性係数を計算（YaneuraOu準拠、クランプなし）
///
/// YaneuraOu: bestMoveInstability = 0.9929 + 1.8519 * totBestMoveChanges / threads.size()
///
/// # Arguments
/// * `tot_best_move_changes` - 最善手変更の累積カウント
/// * `thread_count` - スレッド数（現在は1固定、マルチスレッド対応時に拡張）
pub fn calculate_best_move_instability(tot_best_move_changes: f64, thread_count: usize) -> f64 {
    BEST_MOVE_INSTABILITY_BASE
        + BEST_MOVE_INSTABILITY_FACTOR * tot_best_move_changes / thread_count.max(1) as f64
}

/// fallingEvalを計算（YaneuraOu準拠）
///
/// fallingEval = (11.396 + 2.035 * (best_prev_avg - best) + 0.968 * (iter_value - best)) / 100
/// を [0.5786, 1.6752] にクランプする。
#[inline]
pub fn calculate_falling_eval(best_prev_avg: i32, iter_value: i32, best_value: i32) -> f64 {
    let delta_avg = (best_prev_avg - best_value) as f64;
    let delta_iter = (iter_value - best_value) as f64;
    let eval = (11.396 + 2.035 * delta_avg + 0.968 * delta_iter) / 100.0;
    eval.clamp(0.5786, 1.6752)
}

/// timeReductionを計算（YaneuraOu準拠）
///
/// timeReduction = 0.8 + 0.84 / (1.077 + exp(-0.527 * (depth - (last_best_move_depth + 11))))
/// を返す。
#[inline]
pub fn calculate_time_reduction(completed_depth: i32, last_best_move_depth: i32) -> f64 {
    let k = 0.527;
    let center = last_best_move_depth as f64 + 11.0;
    0.8 + 0.84 / (1.077 + (-k * (completed_depth as f64 - center)).exp())
}

/// nodesEffort を正規化する（YaneuraOu準拠）
#[inline]
pub fn normalize_nodes_effort(effort: f64, nodes_total: u64) -> f64 {
    effort * 100000.0 / nodes_total.max(1) as f64
}

// =============================================================================
// TimeManagement
// =============================================================================

/// 時間管理クラス
///
/// 探索の思考時間を計算し、停止判定を行う。
pub struct TimeManagement {
    /// 探索開始時刻
    start_time: Instant,

    /// 最適思考時間（ミリ秒）
    optimum_time: TimePoint,

    /// 最大思考時間（ミリ秒）
    maximum_time: TimePoint,

    /// 最小思考時間（ミリ秒）
    minimum_time: TimePoint,

    /// 探索終了時刻（start_timeからの経過時間）
    /// 0なら未確定
    search_end: TimePoint,

    /// ponderhit時刻
    ponderhit_time: Instant,

    /// 秒読みに突入しているか
    is_final_push: bool,

    /// 最小思考時間設定
    minimum_thinking_time: TimePoint,

    /// ネットワーク遅延設定
    network_delay: TimePoint,
    /// ネットワーク遅延2（切れ負け対策）
    network_delay2: TimePoint,

    /// SlowMover（百分率）
    slow_mover: i32,

    /// 今回の最大残り時間（NetworkDelay2 減算後）
    remain_time: TimePoint,

    /// 探索停止フラグ（外部から設定される）
    stop: Arc<AtomicBool>,

    /// ponderhit通知フラグ（外部から設定される）
    ponderhit: Arc<AtomicBool>,

    /// 合法手が1つだった場合に500ms上限を再適用するためのフラグ
    single_move_limit: bool,

    /// 前回のtime_reductionを保持（YaneuraOu準拠のreduction計算に使用）
    previous_time_reduction: f64,

    /// Ponderオプション（YaneuraOu準拠）
    usi_ponder: bool,
    stochastic_ponder: bool,

    /// Ponder中に時間を使い切ったフラグ（stopOnPonderhit相当）
    stop_on_ponderhit: bool,

    /// 動的な ponder 状態
    ///
    /// - `go ponder` で開始した場合は `true`
    /// - `ponderhit` を受信したら `false` に落として通常探索へ移行する
    is_pondering: bool,

    /// 直近の停止閾値（min(total_time, maximum_time)を保持）
    last_stop_threshold: Option<TimePoint>,
}

impl TimeManagement {
    /// 新しいTimeManagementを作成
    pub fn new(stop: Arc<AtomicBool>, ponderhit: Arc<AtomicBool>) -> Self {
        let now = Instant::now();
        Self {
            start_time: now,
            optimum_time: 0,
            maximum_time: 0,
            minimum_time: 0,
            search_end: 0,
            ponderhit_time: now,
            is_final_push: false,
            minimum_thinking_time: DEFAULT_MINIMUM_THINKING_TIME,
            network_delay: DEFAULT_NETWORK_DELAY,
            network_delay2: DEFAULT_NETWORK_DELAY2,
            slow_mover: DEFAULT_SLOW_MOVER,
            remain_time: TimePoint::MAX / 2,
            stop,
            ponderhit,
            single_move_limit: false,
            previous_time_reduction: 0.85,
            usi_ponder: false,
            stochastic_ponder: false,
            stop_on_ponderhit: false,
            is_pondering: false,
            last_stop_threshold: None,
        }
    }

    /// オプションを適用（USI setoption 相当）
    pub fn set_options(&mut self, opts: &TimeOptions) {
        self.network_delay = opts.network_delay.max(0);
        self.network_delay2 = opts.network_delay2.max(0);
        self.minimum_thinking_time = opts.minimum_thinking_time.max(MIN_MINIMUM_THINKING_TIME);
        self.slow_mover = opts.slow_mover.clamp(1, 1000);
        self.usi_ponder = opts.usi_ponder;
        self.stochastic_ponder = opts.stochastic_ponder;
    }

    /// 前回の time_reduction をセット（YO準拠の持ち回り用）
    pub fn set_previous_time_reduction(&mut self, value: f64) {
        self.previous_time_reduction = value;
    }

    #[cfg(test)]
    pub fn previous_time_reduction_mut(&mut self) -> &mut f64 {
        &mut self.previous_time_reduction
    }

    /// 現在の time_reduction を取得
    pub fn previous_time_reduction(&self) -> f64 {
        self.previous_time_reduction
    }

    /// is_final_pushゲッター
    pub fn is_final_push(&self) -> bool {
        self.is_final_push
    }

    /// remain_timeゲッター（テスト用）
    #[cfg(test)]
    pub fn remain_time(&self) -> TimePoint {
        self.remain_time
    }

    /// round_up処理（YaneuraOu準拠）
    ///
    /// 1秒単位で繰り上げてdelayを引く。
    /// ただし、remain_timeよりは小さくなるように制限する。
    pub fn round_up(&self, t0: TimePoint) -> TimePoint {
        // 1000で繰り上げる。minimum_thinking_timeが最低値。
        let mut t = ((t0 + 999) / 1000 * 1000).max(self.minimum_thinking_time);

        // network_delayの値を引く
        t = t.saturating_sub(self.network_delay);

        // 元の値より小さいなら、もう1秒使う
        if t < t0 {
            t += 1000;
        }

        // remain_timeを上回ってはならない
        t = t.min(self.remain_time);
        t.max(0)
    }

    /// 今回の思考時間を決定する
    ///
    /// # Arguments
    /// * `limits` - 探索制限
    /// * `us` - 自分の手番
    /// * `ply` - 現在の手数
    /// * `max_moves_to_draw` - 引き分けまでの最大手数
    pub fn init(&mut self, limits: &LimitsType, us: Color, ply: i32, max_moves_to_draw: i32) {
        self.start_time = limits.start_time.unwrap_or_else(Instant::now);
        self.ponderhit_time = self.start_time;
        self.search_end = 0;
        self.is_final_push = false;
        self.is_pondering = limits.ponder;
        self.ponderhit.store(false, Ordering::Relaxed);
        self.single_move_limit = false;
        self.stop_on_ponderhit = false;
        self.last_stop_threshold = None;

        // movetime指定の場合
        if limits.has_movetime() {
            let movetime = limits.movetime;
            self.remain_time = movetime;
            self.optimum_time = movetime;
            self.maximum_time = movetime;
            self.minimum_time = movetime;
            self.search_end = movetime;
            self.last_stop_threshold = Some(movetime);
            return;
        }

        // 時間制御を使わない場合（depth, nodes, infinite等）
        if !limits.use_time_management() {
            self.optimum_time = TimePoint::MAX / 2;
            self.maximum_time = TimePoint::MAX / 2;
            self.remain_time = TimePoint::MAX / 2;
            self.minimum_time = 0;
            return;
        }

        let time_left = limits.time_left(us);
        let increment = limits.increment(us);
        let byoyomi = limits.byoyomi_time(us);

        // NetworkDelay2 を考慮した今回の残り時間
        self.remain_time = (time_left + increment + byoyomi - self.network_delay2).max(100);

        // rtime 指定時はランダム化した固定時間を使用
        if limits.rtime > 0 {
            let mut r = limits.rtime;
            if ply > 0 {
                let max_rand = (r as f64 * 0.5).min(r as f64 * 10.0 / ply as f64);
                if max_rand > 0.0 {
                    let mut rng = rand::rng();
                    let extra = rng.random_range(0..=max_rand as TimePoint);
                    r = r.saturating_add(extra);
                }
            }

            self.remain_time = r;
            self.minimum_time = r;
            self.optimum_time = r;
            self.maximum_time = r;
            self.search_end = r;
            self.last_stop_threshold = Some(r);
            return;
        }

        let max_moves = if max_moves_to_draw > 0 {
            max_moves_to_draw
        } else {
            DEFAULT_MAX_MOVES_TO_DRAW
        };

        // 切れ負けルールか？
        let time_forfeit = increment == 0 && byoyomi == 0;

        // move_horizon の近似 (MoveHorizon = 160 をベースに補正)
        let move_horizon = calculate_move_horizon(time_forfeit, ply);

        // 残り手数 (MTG)
        let mtg = (max_moves - ply + 2).min(move_horizon) / 2;
        if mtg <= 0 {
            self.minimum_time = 500;
            self.optimum_time = 500;
            self.maximum_time = 500;
            return;
        }
        if mtg == 1 {
            self.minimum_time = self.remain_time;
            self.optimum_time = self.remain_time;
            self.maximum_time = self.remain_time;
            return;
        }

        // 最小思考時間 (network_delay を差し引く)
        self.minimum_time = (self.minimum_thinking_time - self.network_delay).max(1000);

        // 初期値は残り時間
        self.optimum_time = self.remain_time;
        self.maximum_time = self.remain_time;

        // remain_estimate = time + inc*mtg + byoyomi*mtg - (mtg+1)*1000
        let mtg_i64 = mtg as TimePoint;
        let mut remain_estimate = time_left
            .saturating_add(increment.saturating_mul(mtg_i64))
            .saturating_add(byoyomi.saturating_mul(mtg_i64));
        remain_estimate = remain_estimate.saturating_sub((mtg_i64 + 1) * 1000);
        if remain_estimate < 0 {
            remain_estimate = 0;
        }

        // optimum: minimum + remain_estimate/mtg
        let t1 = self.minimum_time + remain_estimate / mtg_i64;

        // maximum: minimum + remain_estimate * max_ratio / mtg
        let mut max_ratio: f64 = 5.0;
        if time_forfeit {
            let ratio = (time_left as f64) / (60.0 * 1000.0);
            max_ratio = max_ratio.min(ratio.max(1.0));
        }
        let mut t2 =
            self.minimum_time + (remain_estimate as f64 * max_ratio / mtg_i64 as f64) as TimePoint;
        // maximum は残り時間の30%を上限
        let max_cap = (remain_estimate as f64 * 0.3) as TimePoint;
        t2 = t2.min(max_cap);

        self.optimum_time = t1.min(self.optimum_time);
        self.maximum_time = t2.min(self.maximum_time);

        // SlowMover は YaneuraOu 同様 optimum のみスケールする（秒読みの最終局面は除外）
        self.optimum_time = self.optimum_time * self.slow_mover as i64 / 100;

        // Ponder時調整（YaneuraOu準拠）
        // Ponderが有効でStochastic_Ponderが無効の場合、optimumTimeを25%増やす
        if self.usi_ponder && !self.stochastic_ponder {
            self.optimum_time += self.optimum_time / 4;
        }

        // 秒読みモードでかつ持ち時間が少ない場合は使い切る
        self.is_final_push = false;
        if byoyomi > 0 && time_left < (byoyomi as f64 * 1.2) as TimePoint {
            self.minimum_time = byoyomi + time_left;
            self.optimum_time = byoyomi + time_left;
            self.maximum_time = byoyomi + time_left;
            self.is_final_push = true;
        }

        // round_up は NetworkDelay・minimum_thinking_time・remain_time を考慮する
        self.minimum_time = self.round_up(self.minimum_time);
        self.optimum_time = self.optimum_time.min(self.remain_time).max(1);
        self.maximum_time = self.round_up(self.maximum_time);

        // 最低限の整合性確保
        if self.optimum_time < self.minimum_time {
            self.optimum_time = self.minimum_time;
        }
        if self.maximum_time < self.optimum_time {
            self.maximum_time = self.optimum_time;
        }
    }

    /// 今回の思考時間を決定する（合法手数を考慮）
    ///
    /// # Arguments
    /// * `limits` - 探索制限
    /// * `us` - 自分の手番
    /// * `ply` - 現在の手数
    /// * `max_moves_to_draw` - 引き分けまでの最大手数
    /// * `root_moves_count` - ルートでの合法手の数
    pub fn init_with_root_moves_count(
        &mut self,
        limits: &LimitsType,
        us: Color,
        ply: i32,
        max_moves_to_draw: i32,
        root_moves_count: usize,
    ) {
        // 通常の初期化
        self.init(limits, us, ply, max_moves_to_draw);

        // 合法手が1つの場合は500ms上限
        if root_moves_count == 1 {
            self.apply_single_move_limit();
            self.single_move_limit = true;
        } else {
            self.single_move_limit = false;
        }
    }

    /// 合法手1つの場合の時間制限適用
    ///
    /// YaneuraOu準拠: 視聴者体験を向上させるため、合法手が1つだけの場合に
    /// 使用時間を500ms以下に制限する（実際の制限はtotal_time計算後に適用）
    pub fn apply_single_move_limit(&mut self) {
        self.single_move_limit = true;
    }

    /// 最善手不安定性係数を適用して optimum_time を調整
    ///
    /// YaneuraOu準拠: bestMoveInstability = 0.9929 + 1.8519 * totBestMoveChanges / threads.size()
    ///
    /// # Arguments
    /// * `tot_best_move_changes` - 最善手変更の累積カウント
    /// * `thread_count` - スレッド数（現在は1固定、マルチスレッド対応時に拡張）
    pub fn apply_best_move_instability(&mut self, tot_best_move_changes: f64, thread_count: usize) {
        // 互換メソッドは factor を返す形に変更
        let _ = self.compute_time_factor(1.0, 1.0, tot_best_move_changes, thread_count);
    }

    /// fallingEval / timeReduction / bestMoveInstability の総合係数を計算
    ///
    /// - `falling_eval`      : 評価値の変動度合い（未実装なら1.0を渡す）
    /// - `time_reduction`    : 深さに応じた時間短縮係数（未実装なら1.0を渡す）
    /// - `tot_best_move_changes` : 最善手変更回数の合計（将来は全スレッド合算を thread_count で割る）
    /// - `thread_count`      : スレッド数（並列探索時に利用予定）
    pub fn compute_time_factor(
        &mut self,
        falling_eval: f64,
        time_reduction: f64,
        tot_best_move_changes: f64,
        thread_count: usize,
    ) -> f64 {
        // YaneuraOu準拠: byoyomiモード (is_final_push=true) では時間を変更しない
        // byoyomiは固定時間なので、動的な調整は適用しない
        if self.is_final_push {
            return 1.0;
        }

        let instability = calculate_best_move_instability(tot_best_move_changes, thread_count);
        let reduction =
            (1.4540 + self.previous_time_reduction) / (2.1593 * time_reduction.max(0.0001));
        self.previous_time_reduction = time_reduction;

        falling_eval * reduction * instability
    }

    /// 1イテレーションで使うべき totalTime（YaneuraOu準拠）を計算
    pub fn total_time_for_iteration(
        &mut self,
        falling_eval: f64,
        time_reduction: f64,
        tot_best_move_changes: f64,
        thread_count: usize,
    ) -> f64 {
        let factor = self.compute_time_factor(
            falling_eval,
            time_reduction,
            tot_best_move_changes,
            thread_count,
        );
        self.optimum_time as f64 * factor
    }

    /// イテレーション終了後の時間判定を行い、必要なら search_end を設定
    pub fn apply_iteration_timing(
        &mut self,
        elapsed: TimePoint,
        total_time: f64,
        nodes_effort: f64,
        completed_depth: i32,
    ) {
        let is_pondering = self.is_pondering;
        let effective_elapsed = self.effective_elapsed(elapsed);

        // YaneuraOu: completedDepth>=10 && nodesEffort>=97056 && elapsed > totalTime*0.6540 なら search_end 設定
        if completed_depth >= 10
            && nodes_effort >= 97056.0
            && (effective_elapsed as f64) > total_time * 0.6540
            && !is_pondering
        {
            self.set_search_end(elapsed);
        }

        // min(total_time, maximum_time) を停止閾値として保持（単一合法手の500ms上限も適用）
        let mut stop_threshold =
            (total_time.min(self.maximum_time as f64).ceil() as TimePoint).max(0);
        if self.single_move_limit {
            stop_threshold = stop_threshold.min(SINGLE_MOVE_TIME_LIMIT);
        }
        self.last_stop_threshold = Some(stop_threshold);

        // total_time超過時の処理
        if (effective_elapsed as f64) > total_time.min(self.maximum_time as f64) {
            if is_pondering {
                // ponder中は stop_on_ponderhit を立て、次のチェックで秒切り上げ終了時刻を設定
                self.stop_on_ponderhit = true;
            } else {
                self.set_search_end(elapsed);
            }
        }

        debug!(
            target: "engine_core::search",
            "apply_iteration_timing: elapsed={}ms total_time={:.3} max_time={} min_time={} stop_threshold={:?} search_end={} nodes_effort={:.1} depth={} ponder={} final_push={} single_move_limit={} stop_on_ponderhit={}",
            effective_elapsed,
            total_time,
            self.maximum_time,
            self.minimum_time,
            self.last_stop_threshold,
            self.search_end,
            nodes_effort,
            completed_depth,
            is_pondering,
            self.is_final_push,
            self.single_move_limit,
            self.stop_on_ponderhit,
        );
    }

    /// 最適思考時間を取得
    #[inline]
    pub fn optimum(&self) -> TimePoint {
        self.optimum_time
    }

    /// 最大思考時間を取得
    #[inline]
    pub fn maximum(&self) -> TimePoint {
        self.maximum_time
    }

    /// 最小思考時間を取得
    #[inline]
    pub fn minimum(&self) -> TimePoint {
        self.minimum_time
    }

    /// 探索終了時刻を取得（start_timeからの経過時間、ミリ秒）
    /// 0の場合は未設定
    #[inline]
    pub fn search_end(&self) -> TimePoint {
        self.search_end
    }

    /// search_endをクリアし、ponderhitフラグもリセット
    pub fn reset_search_end(&mut self) {
        self.search_end = 0;
        self.stop_on_ponderhit = false;
        self.last_stop_threshold = None;
    }

    /// stopOnPonderhit相当のフラグ取得
    #[inline]
    pub fn stop_on_ponderhit(&self) -> bool {
        self.stop_on_ponderhit
    }

    /// 現在 ponder 中か（動的状態）
    #[inline]
    pub fn is_pondering(&self) -> bool {
        self.is_pondering
    }

    /// stop_on_ponderhit フラグをクリア（YOのfail-low時相当）
    pub fn reset_stop_on_ponderhit(&mut self) {
        self.stop_on_ponderhit = false;
    }

    /// 探索開始からの経過時間（ミリ秒）
    #[inline]
    pub fn elapsed(&self) -> TimePoint {
        self.start_time.elapsed().as_millis() as TimePoint
    }

    /// ponderhitからの経過時間（ミリ秒）
    #[inline]
    pub fn elapsed_from_ponderhit(&self) -> TimePoint {
        self.ponderhit_time.elapsed().as_millis() as TimePoint
    }

    /// ponderhitが通知されているか
    pub fn take_ponderhit(&self) -> bool {
        self.ponderhit.swap(false, Ordering::Relaxed)
    }

    /// 探索を停止すべきか判定（反復深化の境目で呼び出す）
    ///
    /// YaneuraOu準拠: ノード単位のチェックでは best_move_stable は使わない。
    /// best_move_changes は反復深化の境目での時間計算（apply_best_move_instability）にのみ影響する。
    ///
    /// # Arguments
    /// * `depth` - 現在の探索深さ
    pub fn should_stop(&mut self, depth: i32) -> bool {
        let _ = depth; // 深さ条件はYaneuraOuでは使用しない
        let elapsed = self.elapsed();
        self.should_stop_internal(elapsed)
    }

    /// 探索を即座に停止すべきか判定（時間チェックのみ）
    #[inline]
    pub fn should_stop_immediately(&mut self) -> bool {
        let elapsed = self.elapsed();
        self.should_stop_internal(elapsed)
    }

    /// should_stop/should_stop_immediately の共通判定処理
    fn should_stop_internal(&mut self, elapsed: TimePoint) -> bool {
        // ponderhit までの経過時間を差し引いた「実効経過時間」
        let effective_elapsed = self.effective_elapsed(elapsed);

        // 外部からの停止要求
        if self.stop.load(Ordering::Relaxed) {
            debug!(
                target: "engine_core::search",
                "stop check: external stop elapsed={} effective_elapsed={} search_end={} last_stop_threshold={:?} max_time={}",
                elapsed,
                effective_elapsed,
                self.search_end,
                self.last_stop_threshold,
                self.maximum_time
            );
            return true;
        }

        // ponder中は時間による停止判定を行わない（USI/UCI仕様、YO準拠）
        if self.is_pondering {
            return false;
        }

        // stop_on_ponderhitが立っていて search_end 未設定ならここで設定する（YO準拠）
        if self.search_end == 0 && self.stop_on_ponderhit {
            self.set_search_end(elapsed);
        }

        // search_end が設定されている場合は、それを最優先に使う（YO準拠）
        if self.search_end > 0 {
            if elapsed >= self.search_end {
                debug!(
                    target: "engine_core::search",
                    "stop check: search_end reached elapsed={} search_end={}",
                    elapsed,
                    self.search_end
                );
                return true;
            }
            return false;
        }

        // total_time由来の直近閾値
        if let Some(threshold) = self.last_stop_threshold {
            if effective_elapsed >= threshold {
                debug!(
                    target: "engine_core::search",
                    "stop check: last_stop_threshold reached effective_elapsed={} threshold={}",
                    effective_elapsed,
                    threshold
                );
                return true;
            }
        }

        // 最大時間を超えた（セーフティ）
        if effective_elapsed >= self.maximum_time {
            debug!(
                target: "engine_core::search",
                "stop check: maximum_time reached effective_elapsed={} max_time={}",
                effective_elapsed,
                self.maximum_time
            );
            return true;
        }

        false
    }

    /// ponderhit時の処理（時刻を記録）
    pub fn set_ponderhit(&mut self) {
        self.ponderhit_time = Instant::now();
    }

    /// ponderhit_time までのオフセット（start_time 基準の経過ミリ秒）を取得
    fn ponderhit_offset(&self) -> TimePoint {
        if self.ponderhit_time >= self.start_time {
            self.ponderhit_time.duration_since(self.start_time).as_millis() as TimePoint
        } else {
            0
        }
    }

    /// start_time 基準の経過時間から、ponderhit 前の消費時間を差し引いた実効経過時間を計算
    fn effective_elapsed(&self, elapsed_raw: TimePoint) -> TimePoint {
        elapsed_raw.saturating_sub(self.ponderhit_offset()).max(0)
    }

    /// ponderhitを検出した際の処理（YO準拠）
    ///
    /// - `ponderhit_time` を更新し、以後の `set_search_end()` の秒境界切り上げ計算に反映する。
    /// - ponder状態を解除して通常探索へ移行する（`is_pondering=false`）。
    /// - ponder中に時間を使い切っていた場合（`stop_on_ponderhit=true`）のみ、停止時刻を確定させる。
    pub fn on_ponderhit(&mut self) {
        // すでに通常探索なら時間基準を動かさない
        if !self.is_pondering {
            self.ponderhit.store(false, Ordering::Relaxed);
            return;
        }

        self.set_ponderhit();
        self.is_pondering = false;
        self.last_stop_threshold = None;

        // ponder中に時間を使い切っていた場合のみ、終了時刻を確定させる（YOの stopOnPonderhit 相当）
        if self.search_end == 0 && self.stop_on_ponderhit {
            let elapsed = self.elapsed();
            self.set_search_end(elapsed);
        }
        self.ponderhit.store(false, Ordering::Relaxed);
    }

    /// 探索終了時刻を設定（YaneuraOu準拠、秒境界に切り上げ）
    ///
    /// YaneuraOu timeman.cpp:314-341 の実装を再現
    ///
    /// # Arguments
    /// * `elapsed_ms` - 探索開始からの経過時間（ミリ秒）
    ///
    /// # 動作
    /// - 経過時間と最小思考時間の大きい方を採用
    /// - 秒境界に切り上げ
    /// - ponderhit時間を考慮した調整
    pub fn set_search_end(&mut self, elapsed_ms: TimePoint) {
        // start_time と ponderhit_time の差分（通常は0、ponder時のみ非0）
        // ponderhit_time は init() で start_time に設定されるため、通常の探索では duration = 0
        let duration_start_to_ponderhit = self.ponderhit_offset();

        // YaneuraOuのロジックを完全再現
        // TimePoint t1 = e + startTime - ponderhitTime;
        // elapsed_ms は start_time からの経過なので、ponderhit調整が必要
        let t1 = elapsed_ms.saturating_sub(duration_start_to_ponderhit);

        // TimePoint t2 = isFinalPush ? minimum() : minimum() + startTime - ponderhitTime;
        let t2 = if self.is_final_push {
            self.minimum_time
        } else {
            self.minimum_time.saturating_sub(duration_start_to_ponderhit)
        };

        let max_time = std::cmp::max(t1, t2);
        let rounded = self.round_up(max_time);

        // search_end = round_up(std::max(t1, t2)) + ponderhitTime - startTime;
        self.search_end = rounded.saturating_add(duration_start_to_ponderhit);
    }

    /// 外部から停止を要求
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// 停止フラグをリセット
    pub fn reset_stop(&self) {
        self.stop.store(false, Ordering::Relaxed);
    }

    /// 停止要求が出ているか
    #[inline]
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

impl Default for TimeManagement {
    fn default() -> Self {
        Self::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)))
    }
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn create_time_manager() -> TimeManagement {
        TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn test_time_manager_rtime_sets_budget() {
        let mut tm = create_time_manager();
        let mut limits = LimitsType::new();
        limits.rtime = 2500;
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, DEFAULT_MAX_MOVES_TO_DRAW);

        assert_eq!(tm.minimum(), 2500);
        assert_eq!(tm.optimum(), 2500);
        assert_eq!(tm.maximum(), 2500);
        assert_eq!(tm.search_end(), 2500, "rtime は固定時間として search_end も設定されるべき");
    }

    #[test]
    fn test_optimum_scales_with_ponder_option() {
        let mut base = create_time_manager();
        let mut ponder = create_time_manager();

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 60_000;
        limits.set_start_time();

        base.init(&limits, Color::Black, 0, DEFAULT_MAX_MOVES_TO_DRAW);

        ponder.set_options(&TimeOptions {
            usi_ponder: true,
            stochastic_ponder: false,
            ..TimeOptions::default()
        });
        ponder.init(&limits, Color::Black, 0, DEFAULT_MAX_MOVES_TO_DRAW);

        assert_eq!(ponder.optimum(), base.optimum() + base.optimum() / 4);
    }

    #[test]
    fn test_compute_time_factor_uses_yaneura_coeffs() {
        let mut tm = create_time_manager();
        tm.optimum_time = 1000;
        tm.maximum_time = 2000;
        tm.remain_time = TimePoint::MAX / 4;
        tm.single_move_limit = false;
        tm.is_final_push = false;
        tm.previous_time_reduction = 0.85;

        let falling_eval = 1.1;
        let time_reduction = 1.2;
        let tot_best_move_changes = 0.0;
        let thread_count = 1;

        let factor = tm.compute_time_factor(
            falling_eval,
            time_reduction,
            tot_best_move_changes,
            thread_count,
        );

        let instability = 0.9929 + 1.8519 * tot_best_move_changes / thread_count as f64;
        let reduction = (1.4540 + 0.85) / (2.1593 * time_reduction);
        let expected_factor = falling_eval * reduction * instability;

        assert!((factor - expected_factor).abs() < 1e-9);
        // optimum/maximumは変化しない
        assert_eq!(tm.optimum(), 1000);
        assert_eq!(tm.maximum(), 2000);
    }

    #[test]
    fn test_previous_time_reduction_roundtrip() {
        let mut tm = create_time_manager();
        tm.set_previous_time_reduction(0.42);
        assert!((tm.previous_time_reduction() - 0.42).abs() < 1e-9);
    }

    #[test]
    fn test_previous_time_reduction_is_preserved_through_init() {
        let mut tm = create_time_manager();
        tm.set_previous_time_reduction(0.42);

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 60_000;
        limits.set_start_time();

        tm.init(&limits, Color::Black, 10, DEFAULT_MAX_MOVES_TO_DRAW);

        assert!(
            (tm.previous_time_reduction() - 0.42).abs() < 1e-9,
            "init() で previous_time_reduction がリセットされないことを保証する"
        );
    }

    #[test]
    fn test_apply_iteration_timing_sets_search_end_on_effort() {
        let mut tm = create_time_manager();
        tm.optimum_time = 1000;
        tm.maximum_time = 2000;
        tm.remain_time = 5000;
        tm.minimum_time = 500;
        tm.search_end = 0;

        tm.apply_iteration_timing(1200, 1000.0, 98000.0, 12);

        assert!(tm.search_end() > 0, "search_end should be set when nodes_effort threshold hit");
    }

    #[test]
    fn test_time_manager_init_no_time_management() {
        let mut tm = create_time_manager();
        let mut limits = LimitsType::new();
        limits.depth = 10; // 深さ固定

        tm.init(&limits, Color::Black, 0, 256);

        // 時間制御しない場合は非常に長い時間が設定される
        assert!(tm.optimum() > 1_000_000_000);
        assert!(tm.maximum() > 1_000_000_000);
    }

    #[test]
    fn test_time_manager_init_with_time() {
        let mut tm = create_time_manager();
        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 60000; // 1分
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, 256);

        assert!(tm.optimum() > 0);
        assert!(tm.maximum() >= tm.optimum());
        assert!(tm.maximum() <= 60000);
    }

    #[test]
    fn test_time_manager_init_byoyomi() {
        let mut tm = create_time_manager();
        let mut limits = LimitsType::new();
        limits.byoyomi[Color::Black.index()] = 30000; // 30秒
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, 256);

        assert!(tm.optimum() > 0);
        assert!(tm.optimum() < 30000);
    }

    #[test]
    fn test_time_manager_elapsed() {
        let mut tm = create_time_manager();
        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 60000;
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, 256);

        // 少し待つ
        std::thread::sleep(std::time::Duration::from_millis(10));

        let elapsed = tm.elapsed();
        assert!(elapsed >= 10);
        assert!(elapsed < 1000);
    }

    #[test]
    fn test_time_manager_should_stop() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut tm = TimeManagement::new(Arc::clone(&stop), Arc::new(AtomicBool::new(false)));

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 100; // 非常に短い時間
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, 256);

        // 最初は停止しない
        assert!(!stop.load(Ordering::Relaxed));

        // 外部から停止を要求
        stop.store(true, Ordering::Relaxed);
        assert!(tm.should_stop(5));
    }

    #[test]
    fn test_stop_on_ponderhit_sets_search_end_when_checked() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut tm = TimeManagement::new(Arc::clone(&stop), Arc::new(AtomicBool::new(false)));

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 5000;
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, DEFAULT_MAX_MOVES_TO_DRAW);

        // 疑似的にponderhitフラグを立て、経過時間を進める
        tm.stop_on_ponderhit = true;
        tm.start_time = Instant::now() - std::time::Duration::from_millis(1200);
        let before = tm.search_end();
        let should_stop = tm.should_stop(5);
        assert!(
            tm.search_end() > before,
            "search_end should be set when stop_on_ponderhit is set"
        );
        // search_endに到達していなければshould_stopはfalseのまま
        assert!(!should_stop || tm.elapsed() >= tm.search_end());
    }

    #[test]
    fn test_last_stop_threshold_is_used() {
        let mut tm = create_time_manager();
        tm.maximum_time = 5000;
        tm.optimum_time = 1000;
        tm.last_stop_threshold = Some(1500);
        tm.start_time = Instant::now() - Duration::from_millis(1600);
        tm.ponderhit_time = tm.start_time;
        assert!(tm.should_stop(5), "elapsed beyond last_stop_threshold should stop");

        tm.search_end = 0;
        tm.last_stop_threshold = Some(2000);
        tm.start_time = Instant::now() - Duration::from_millis(500);
        tm.ponderhit_time = tm.start_time;
        assert!(!tm.should_stop(5), "elapsed below threshold should continue");
    }

    #[test]
    fn test_time_manager_round_up() {
        let tm = create_time_manager();

        // minimum_thinking_time=2000, network_delay=120, remain_timeは十分に大きい
        let result = tm.round_up(1);
        assert_eq!(result, 1880);

        // 500ms -> minimum_thinking_time に引き上げた上で network_delay を差し引く
        let result = tm.round_up(500);
        assert_eq!(result, 1880);

        // 1001ms -> 2秒を切り上げるが minimum_thinking_time が優先
        let result = tm.round_up(1001);
        assert_eq!(result, 1880);
    }

    #[test]
    fn test_round_up_respects_minimum_lower_bound() {
        let mut tm = create_time_manager();
        tm.set_options(&TimeOptions {
            network_delay: 120,
            network_delay2: 1120,
            minimum_thinking_time: 1000,
            slow_mover: 100,
            ..TimeOptions::default()
        });

        assert_eq!(tm.round_up(1), 880);
    }

    #[test]
    fn test_time_manager_on_ponderhit_switches_off_ponder_without_forcing_stop() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut tm = TimeManagement::new(Arc::clone(&stop), Arc::new(AtomicBool::new(false)));

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 5000; // 5秒
        limits.ponder = true;
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, 256);
        tm.last_stop_threshold = Some(1);
        tm.on_ponderhit();

        assert!(!tm.is_pondering(), "ponderhit後は通常探索へ移行するべき");
        assert_eq!(tm.search_end(), 0, "stop_on_ponderhit が無ければ search_end は確定しない");
        assert!(tm.last_stop_threshold.is_none(), "ponderhitで停止閾値をリセットする");
    }

    #[test]
    fn test_ponderhit_does_not_consume_budget_from_long_ponder() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut tm = TimeManagement::new(Arc::clone(&stop), Arc::new(AtomicBool::new(false)));

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 60000; // 1分
        limits.ponder = true;
        limits.start_time = Some(Instant::now() - Duration::from_millis(20_000));

        tm.init(&limits, Color::Black, 0, DEFAULT_MAX_MOVES_TO_DRAW);

        // ponderhitを受信して通常探索へ移行
        tm.on_ponderhit();
        assert!(!tm.is_pondering(), "ponderhit後は通常探索に切り替わる");

        let raw_elapsed = tm.elapsed();
        tm.apply_iteration_timing(raw_elapsed, 5000.0, 0.0, 12);

        assert_eq!(tm.search_end(), 0, "stop_on_ponderhitが無ければ search_end は確定しない");
        assert!(
            !tm.should_stop_immediately(),
            "ponderhit後は ponder 前の経過時間に引きずられず継続できるべき"
        );
    }

    #[test]
    fn test_on_ponderhit_ignored_when_not_pondering() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut tm = TimeManagement::new(Arc::clone(&stop), Arc::new(AtomicBool::new(false)));

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 60000;
        limits.ponder = false;
        limits.start_time = Some(Instant::now() - Duration::from_millis(1500));

        tm.init(&limits, Color::Black, 0, DEFAULT_MAX_MOVES_TO_DRAW);
        let before_elapsed = tm.elapsed_from_ponderhit();

        tm.on_ponderhit(); // 非ponder時の不正通知を無害化

        let after_elapsed = tm.elapsed_from_ponderhit();
        assert!(after_elapsed >= before_elapsed, "非ponder時は時間基準をリセットしない");
        assert_eq!(tm.search_end(), 0, "search_endを確定させない");
        assert!(!tm.stop_on_ponderhit(), "stop_on_ponderhitも変更しない");
        assert!(!tm.is_pondering(), "状態は通常探索のまま");
    }

    #[test]
    fn test_on_ponderhit_final_push_respects_minimum() {
        let mut tm = create_time_manager();
        let mut limits = LimitsType::new();
        limits.byoyomi[Color::Black.index()] = 4000;
        limits.ponder = true;
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, DEFAULT_MAX_MOVES_TO_DRAW);
        tm.stop_on_ponderhit = true;
        tm.on_ponderhit();

        assert!(
            tm.search_end() >= tm.round_up(tm.minimum()),
            "search_end {} should be >= rounded minimum {}",
            tm.search_end(),
            tm.round_up(tm.minimum())
        );
    }

    #[test]
    fn test_apply_iteration_timing_depth_gate() {
        let mut tm = create_time_manager();
        tm.optimum_time = 1000;
        tm.maximum_time = 2000;
        tm.remain_time = 5000;
        tm.minimum_time = 500;
        tm.search_end = 0;

        // depth未満では設定されない（stop_threshold 未満の経過時間で確認）
        tm.apply_iteration_timing(900, 1000.0, 98000.0, 8);
        assert_eq!(tm.search_end(), 0);

        // depth満たすと設定される
        tm.apply_iteration_timing(1200, 1000.0, 98000.0, 12);
        assert!(tm.search_end() >= tm.round_up(tm.minimum()));
    }

    #[test]
    fn test_round_up_uses_remain_time_and_delay() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut tm = TimeManagement::new(Arc::clone(&stop), Arc::new(AtomicBool::new(false)));
        tm.set_options(&TimeOptions {
            network_delay: 120,
            network_delay2: 1120,
            minimum_thinking_time: 2000,
            slow_mover: 100,
            usi_ponder: false,
            stochastic_ponder: false,
        });

        let mut limits = LimitsType::new();
        limits.byoyomi[Color::Black.index()] = 5000;
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, 256);

        // YaneuraOu: remain_time = 5000 - 1120 = 3880
        assert_eq!(tm.optimum(), 3880);
        assert_eq!(tm.maximum(), 3880);
        assert_eq!(tm.minimum(), 3880);
    }

    #[test]
    fn test_network_delay2_reduces_time_budget() {
        let mut tm_base = create_time_manager();
        tm_base.set_options(&TimeOptions {
            network_delay: 0,
            network_delay2: 0,
            minimum_thinking_time: 2000,
            slow_mover: 100,
            usi_ponder: false,
            stochastic_ponder: false,
        });

        let mut tm_delay = create_time_manager();
        tm_delay.set_options(&TimeOptions {
            network_delay: 0,
            network_delay2: 2000,
            minimum_thinking_time: 2000,
            slow_mover: 100,
            usi_ponder: false,
            stochastic_ponder: false,
        });

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 10_000;
        limits.set_start_time();

        tm_base.init(&limits, Color::Black, 0, 256);
        tm_delay.init(&limits, Color::Black, 0, 256);

        assert!(
            tm_delay.optimum() <= tm_base.optimum(),
            "network_delay2 should not increase optimum: base={}, delay={}",
            tm_base.optimum(),
            tm_delay.optimum()
        );
    }

    #[test]
    fn test_slow_mover_scales_time() {
        let mut tm_base = create_time_manager();
        let mut tm_slow = create_time_manager();

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 60_000;
        limits.set_start_time();

        tm_base.init(&limits, Color::Black, 0, 256);

        tm_slow.set_options(&TimeOptions {
            network_delay: 120,
            network_delay2: 1120,
            minimum_thinking_time: 2000,
            slow_mover: 200, // 2倍
            usi_ponder: false,
            stochastic_ponder: false,
        });
        tm_slow.init(&limits, Color::Black, 0, 256);

        assert!(
            tm_slow.optimum() > tm_base.optimum(),
            "slow mover should increase optimum: base={}, slow={}",
            tm_base.optimum(),
            tm_slow.optimum()
        );
    }
}
