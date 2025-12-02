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

// 深い探索(GPU/ネットワーク待ちが長い環境)用プリセット。
// Cargo feature "deep" で有効化すると default() の遅延マージンが 400/1400ms になる。
#[cfg(feature = "deep")]
const DEFAULT_NETWORK_DELAY: TimePoint = 400;
#[cfg(feature = "deep")]
const DEFAULT_NETWORK_DELAY2: TimePoint = 1400;

#[cfg(not(feature = "deep"))]
const DEFAULT_NETWORK_DELAY: TimePoint = 120;
#[cfg(not(feature = "deep"))]
const DEFAULT_NETWORK_DELAY2: TimePoint = 1120;

impl Default for TimeOptions {
    fn default() -> Self {
        // YaneuraOu準拠のデフォルト値
        Self {
            network_delay: DEFAULT_NETWORK_DELAY,
            network_delay2: DEFAULT_NETWORK_DELAY2,
            minimum_thinking_time: 2000,
            slow_mover: 100,
            usi_ponder: false,
            stochastic_ponder: false,
        }
    }
}

impl TimeOptions {
    /// Deep版のデフォルトを取得（YaneuraOuのYANEURAOU_ENGINE_DEEP相当）
    pub fn deep_defaults() -> Self {
        Self {
            network_delay: 400,
            network_delay2: 1400,
            minimum_thinking_time: 2000,
            slow_mover: 100,
            usi_ponder: false,
            stochastic_ponder: false,
        }
    }
}
