//! 時間管理（TimeManagement）
//!
//! 使用可能な最大時間、対局の手数、その他のパラメータに応じて、
//! 思考に費やす最適な時間を計算する。

use super::{LimitsType, TimePoint};
use crate::types::Color;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

// =============================================================================
// 定数
// =============================================================================

/// デフォルトの最小思考時間（ミリ秒）
const DEFAULT_MINIMUM_THINKING_TIME: TimePoint = 20;

/// デフォルトのネットワーク遅延（ミリ秒）
const DEFAULT_NETWORK_DELAY: TimePoint = 120;

/// 引き分けまでの最大手数のデフォルト値
const DEFAULT_MAX_MOVES_TO_DRAW: i32 = 512;

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

    /// 探索停止フラグ（外部から設定される）
    stop: Arc<AtomicBool>,
}

impl TimeManagement {
    /// 新しいTimeManagementを作成
    pub fn new(stop: Arc<AtomicBool>) -> Self {
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
            stop,
        }
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

        // 時間制御を使わない場合
        if !limits.use_time_management() {
            self.optimum_time = TimePoint::MAX / 2;
            self.maximum_time = TimePoint::MAX / 2;
            self.minimum_time = 0;
            return;
        }

        let time_left = limits.time_left(us);
        let increment = limits.increment(us);
        let byoyomi = limits.byoyomi_time(us);

        // 残り手数の推定
        let moves_to_go = if max_moves_to_draw > 0 {
            ((max_moves_to_draw - ply) / 2).max(1) as TimePoint
        } else {
            ((DEFAULT_MAX_MOVES_TO_DRAW - ply) / 2).max(1) as TimePoint
        };

        // 持ち時間がある場合
        if time_left > 0 {
            self.calculate_time_with_time_left(time_left, increment, byoyomi, moves_to_go);
        }
        // 秒読みのみの場合
        else if byoyomi > 0 {
            self.calculate_time_byoyomi_only(byoyomi);
        }
        // それ以外（フリータイム）
        else {
            self.optimum_time = 1000; // 1秒
            self.maximum_time = 10000; // 10秒
            self.minimum_time = 100;
        }

        // ネットワーク遅延を考慮
        self.optimum_time = (self.optimum_time - self.network_delay).max(1);
        self.maximum_time = (self.maximum_time - self.network_delay).max(1);
        self.minimum_time = self.minimum_time.min(self.optimum_time);
    }

    /// 持ち時間がある場合の思考時間計算
    fn calculate_time_with_time_left(
        &mut self,
        time_left: TimePoint,
        increment: TimePoint,
        byoyomi: TimePoint,
        moves_to_go: TimePoint,
    ) {
        // 基本的な時間配分（YaneuraOu準拠）
        // optimum = time_left / moves_to_go + increment
        // maximum = time_left * 0.8 + increment

        let base_time = time_left / moves_to_go;
        let optimum = base_time + increment;
        let maximum = (time_left * 8 / 10) + increment;

        // 秒読みがある場合は少し余裕を持たせる
        if byoyomi > 0 {
            self.optimum_time = optimum.min(time_left + byoyomi - self.minimum_thinking_time);
            self.maximum_time = maximum.min(time_left + byoyomi - self.minimum_thinking_time / 2);
        } else {
            self.optimum_time = optimum.min(time_left - self.minimum_thinking_time);
            self.maximum_time = maximum.min(time_left - self.minimum_thinking_time / 2);
        }

        self.minimum_time = self.minimum_thinking_time;

        // 時間が少ない場合の調整
        if self.optimum_time < self.minimum_time {
            self.optimum_time = self.minimum_time;
        }
        if self.maximum_time < self.optimum_time {
            self.maximum_time = self.optimum_time;
        }
    }

    /// 秒読みのみの場合の思考時間計算
    fn calculate_time_byoyomi_only(&mut self, byoyomi: TimePoint) {
        // 秒読みの場合は秒読み時間をフルに使う
        self.optimum_time = byoyomi - self.network_delay;
        self.maximum_time = byoyomi - self.network_delay / 2;
        self.minimum_time = self.minimum_thinking_time;
        self.is_final_push = true;
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

    /// 探索を停止すべきか判定
    ///
    /// # Arguments
    /// * `depth` - 現在の探索深さ
    /// * `best_move_stable` - 最善手が安定しているか
    pub fn should_stop(&self, depth: i32, best_move_stable: bool) -> bool {
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

        // 最適時間を超えていて、最善手が安定している
        if elapsed >= self.optimum_time && best_move_stable && depth > 4 {
            return true;
        }

        // 最適時間の80%を超えていて、深さが十分
        if elapsed >= self.optimum_time * 8 / 10 && depth > 10 {
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

    /// ponderhit時の処理
    pub fn set_ponderhit(&mut self) {
        self.ponderhit_time = Instant::now();
    }

    /// 探索終了時刻を設定
    pub fn set_search_end(&mut self, end_time: TimePoint) {
        self.search_end = end_time;
    }

    /// 外部から停止を要求
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// 停止フラグをリセット
    pub fn reset_stop(&self) {
        self.stop.store(false, Ordering::Relaxed);
    }

    /// 秒単位で切り上げ（ネットワーク遅延を考慮）
    pub fn round_up(&self, t: TimePoint) -> TimePoint {
        let with_delay = t + self.network_delay;
        // 秒単位で切り上げ
        ((with_delay + 999) / 1000) * 1000
    }
}

impl Default for TimeManagement {
    fn default() -> Self {
        Self::new(Arc::new(AtomicBool::new(false)))
    }
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_time_manager() -> TimeManagement {
        TimeManagement::new(Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn test_time_manager_default() {
        let tm = create_time_manager();
        assert_eq!(tm.optimum(), 0);
        assert_eq!(tm.maximum(), 0);
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
        assert!(tm.maximum() > tm.optimum());
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
        let mut tm = TimeManagement::new(Arc::clone(&stop));

        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 100; // 非常に短い時間
        limits.set_start_time();

        tm.init(&limits, Color::Black, 0, 256);

        // 最初は停止しない
        assert!(!stop.load(Ordering::Relaxed));

        // 外部から停止を要求
        stop.store(true, Ordering::Relaxed);
        assert!(tm.should_stop(5, false));
    }

    #[test]
    fn test_time_manager_round_up() {
        let tm = create_time_manager();

        // 1ms -> 1秒 + ネットワーク遅延
        let result = tm.round_up(1);
        assert_eq!(result, 1000);

        // 500ms -> 1秒
        let result = tm.round_up(500);
        assert_eq!(result, 1000);

        // 1001ms -> 2秒
        let result = tm.round_up(1001);
        assert_eq!(result, 2000);
    }
}
