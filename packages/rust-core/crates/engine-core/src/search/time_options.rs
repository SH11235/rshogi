//! 時間管理オプション
use super::TimePoint;

/// 時間管理に関するオプション（USI setoption相当）
#[derive(Clone, Copy, Debug)]
pub struct TimeOptions {
    pub network_delay: TimePoint,
    pub network_delay2: TimePoint,
    pub minimum_thinking_time: TimePoint,
    pub slow_mover: i32,
    pub usi_ponder: bool,
    pub stochastic_ponder: bool,
}

impl Default for TimeOptions {
    fn default() -> Self {
        // YaneuraOu準拠のデフォルト値
        Self {
            network_delay: 120,
            network_delay2: 1120,
            minimum_thinking_time: 2000,
            slow_mover: 100,
            usi_ponder: false,
            stochastic_ponder: false,
        }
    }
}
