//! 時間管理（TimeManagement）
//!
//! 使用可能な最大時間、対局の手数、その他のパラメータに応じて、
//! 思考に費やす最適な時間を計算する。

use super::{LimitsType, TimeOptions, TimePoint};
use crate::types::Color;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

// =============================================================================
// ヘルパー関数
// =============================================================================

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

/// 引き分けまでの最大手数のデフォルト値
const DEFAULT_MAX_MOVES_TO_DRAW: i32 = 512;

/// 合法手1つの場合の時間上限（ミリ秒）- YaneuraOu準拠
const SINGLE_MOVE_TIME_LIMIT: TimePoint = 500;

/// 最善手不安定性係数の定数 - YaneuraOu準拠
/// bestMoveInstability = BASE + FACTOR * totBestMoveChanges / threads.size()
/// 注: クランプなし（YaneuraOu準拠）
const BEST_MOVE_INSTABILITY_BASE: f64 = 1.04;
const BEST_MOVE_INSTABILITY_FACTOR: f64 = 1.8956;

// =============================================================================
// 公開関数
// =============================================================================

/// 最善手不安定性係数を計算（YaneuraOu準拠、クランプなし）
///
/// YaneuraOu: bestMoveInstability = 0.9929 + 1.8519 * totBestMoveChanges / threads.size()
///
/// # Arguments
/// * `tot_best_move_changes` - 最善手変更の累積カウント
/// * `thread_count` - スレッド数（現在は1固定、マルチスレッド対応時に拡張）
pub fn calculate_best_move_instability(tot_best_move_changes: f64, thread_count: usize) -> f64 {
    BEST_MOVE_INSTABILITY_BASE
        + BEST_MOVE_INSTABILITY_FACTOR * tot_best_move_changes / thread_count as f64
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
            previous_time_reduction: 1.0,
        }
    }

    /// オプションを適用（USI setoption 相当）
    pub fn set_options(&mut self, opts: &TimeOptions) {
        self.network_delay = opts.network_delay.max(0);
        self.network_delay2 = opts.network_delay2.max(0);
        self.minimum_thinking_time = opts.minimum_thinking_time.max(0);
        self.slow_mover = opts.slow_mover.clamp(1, 1000);
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
        self.ponderhit.store(false, Ordering::Relaxed);
        self.single_move_limit = false;
        self.previous_time_reduction = 1.0;

        // movetime指定の場合
        if limits.has_movetime() {
            let movetime = limits.movetime;
            self.remain_time = movetime;
            self.optimum_time = movetime;
            self.maximum_time = movetime;
            self.minimum_time = movetime;
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

        // --- 以降、YaneuraOuのinit_ロジックを反映 ---

        let max_moves = if max_moves_to_draw > 0 {
            max_moves_to_draw
        } else {
            DEFAULT_MAX_MOVES_TO_DRAW
        };

        // 切れ負けルールか？
        let time_forfeit = increment == 0 && byoyomi == 0;

        // move_horizon の近似 (MoveHorizon = 160 をベースに補正)
        let move_horizon = if time_forfeit {
            160 + 40 - ply.min(40)
        } else {
            160 + 20 - ply.min(80)
        };

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
    /// 使用時間を500ms以下に制限する
    pub fn apply_single_move_limit(&mut self) {
        self.optimum_time = self.optimum_time.min(SINGLE_MOVE_TIME_LIMIT);
        self.maximum_time = self.maximum_time.min(SINGLE_MOVE_TIME_LIMIT);
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
        self.apply_time_multipliers(1.0, 1.0, tot_best_move_changes, thread_count);
    }

    /// fallingEval / timeReduction / bestMoveInstability をまとめて適用
    ///
    /// - `falling_eval`      : 評価値の変動度合い（未実装なら1.0を渡す）
    /// - `time_reduction`    : 深さに応じた時間短縮係数（未実装なら1.0を渡す）
    /// - `tot_best_move_changes` : 最善手変更回数の合計（将来は全スレッド合算を thread_count で割る）
    /// - `thread_count`      : スレッド数（並列探索時に利用予定）
    pub fn apply_time_multipliers(
        &mut self,
        falling_eval: f64,
        time_reduction: f64,
        tot_best_move_changes: f64,
        thread_count: usize,
    ) {
        // YaneuraOu準拠: byoyomiモード (is_final_push=true) では時間を変更しない
        // byoyomiは固定時間なので、動的な調整は適用しない
        if self.is_final_push {
            return;
        }

        let instability = calculate_best_move_instability(tot_best_move_changes, thread_count);
        let reduction =
            (1.455 + self.previous_time_reduction) / (2.2375 * time_reduction.max(0.0001));
        self.previous_time_reduction = reduction;

        let factor = falling_eval * reduction * instability;

        self.optimum_time = (self.optimum_time as f64 * factor) as TimePoint;
        self.maximum_time = (self.maximum_time as f64 * factor) as TimePoint;

        if self.single_move_limit {
            self.apply_single_move_limit();
        }

        self.optimum_time = self.optimum_time.min(self.remain_time).max(1);
        self.maximum_time = self.maximum_time.min(self.remain_time).max(self.optimum_time);
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
    pub fn should_stop(&self, depth: i32) -> bool {
        // 外部からの停止要求
        if self.stop.load(Ordering::Relaxed) {
            return true;
        }

        let elapsed = self.elapsed();

        // search_endが設定されていればそれで判定
        if self.search_end > 0 && elapsed >= self.search_end {
            return true;
        }

        // 最大時間を超えた
        if elapsed >= self.maximum_time {
            return true;
        }

        // 最適時間を超えていれば停止
        if elapsed >= self.optimum_time && depth > 4 {
            return true;
        }

        // 最適時間の80%を超えていて、深さが十分
        if elapsed >= self.optimum_time.saturating_mul(8) / 10 && depth > 10 {
            return true;
        }

        false
    }

    /// 探索を即座に停止すべきか判定（時間チェックのみ）
    #[inline]
    pub fn should_stop_immediately(&self) -> bool {
        if self.stop.load(Ordering::Relaxed) {
            return true;
        }

        let elapsed = self.elapsed();

        if self.search_end > 0 && elapsed >= self.search_end {
            return true;
        }

        elapsed >= self.maximum_time
    }

    /// ponderhit時の処理（時刻を記録）
    pub fn set_ponderhit(&mut self) {
        self.ponderhit_time = Instant::now();
    }

    /// ponderhitを検出した際に、現在時刻からminimum分を確保するよう終了時刻を再設定
    ///
    /// YaneuraOuの `TimeManagement::set_search_end()` を簡略化したもの。
    /// e : go開始からの経過時間（現在のelapsed）
    /// t1: ponder開始→ponderhitまでの消費時間を差し引いた思考時間
    /// t2: 秒読み中なら minimum、それ以外なら minimum から ponderhit までを差し引いたもの
    /// search_end: round_up(max(t1, t2)) + ponderhitまでの経過時間
    pub fn on_ponderhit(&mut self) {
        self.set_ponderhit();
        let elapsed = self.elapsed();
        let from_ponderhit = self.elapsed_from_ponderhit();

        let t1 = elapsed.saturating_sub(from_ponderhit);
        let t2 = if self.is_final_push {
            self.minimum_time
        } else {
            self.minimum_time.saturating_sub(from_ponderhit)
        };

        let candidate = t1.max(t2);
        self.search_end = self.round_up(candidate).saturating_add(from_ponderhit);
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
        // ponderhit_time は init() で start_time に設定されるため、
        // 通常の探索では duration = 0
        let duration_start_to_ponderhit = if self.ponderhit_time >= self.start_time {
            self.ponderhit_time.duration_since(self.start_time).as_millis() as TimePoint
        } else {
            0
        };

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

    /// 秒単位で切り上げ（ネットワーク遅延・minimum_thinking_time・remain_timeを考慮）
    pub fn round_up(&self, t: TimePoint) -> TimePoint {
        // 1000ms単位に切り上げ
        let mut rounded = ((t + 999) / 1000) * 1000;
        // 最小思考時間を下回らない
        rounded = rounded.max(self.minimum_thinking_time);
        // NetworkDelay を前倒しで差し引く
        rounded = rounded.saturating_sub(self.network_delay);
        // 差し引きで元より小さくなるならもう1秒追加
        if rounded < t {
            rounded += 1000;
        }
        // remain_time を超えないようにする
        rounded.min(self.remain_time)
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

    fn create_time_manager() -> TimeManagement {
        TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)))
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
    fn test_time_manager_on_ponderhit_sets_search_end() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut tm = TimeManagement::new(Arc::clone(&stop), Arc::new(AtomicBool::new(false)));

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 5000; // 5秒
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, 256);
        std::thread::sleep(std::time::Duration::from_millis(5));
        tm.on_ponderhit();

        // round_up(minimum) は network_delay を考慮して切り上げるので、search_end はその値以上になる
        assert!(
            tm.search_end >= tm.round_up(tm.minimum()),
            "search_end {} should be >= rounded minimum {}",
            tm.search_end,
            tm.round_up(tm.minimum())
        );
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
        });

        let mut tm_delay = create_time_manager();
        tm_delay.set_options(&TimeOptions {
            network_delay: 0,
            network_delay2: 2000,
            minimum_thinking_time: 2000,
            slow_mover: 100,
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
