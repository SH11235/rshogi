//! Type definitions for time management

use super::TimeParameters;

/// Time control settings for a game
#[derive(Debug, Clone)]
pub enum TimeControl {
    /// Fischer time control: base time + increment per move
    Fischer {
        white_ms: u64,
        black_ms: u64,
        increment_ms: u64,
    },
    /// Fixed time per move
    FixedTime { ms_per_move: u64 },
    /// Fixed nodes per move
    FixedNodes { nodes: u64 },
    /// Byoyomi (Japanese overtime)
    Byoyomi {
        main_time_ms: u64, // Main time
        byoyomi_ms: u64,   // Time per period
        periods: u32,      // Number of periods
    },
    /// No time limit
    Infinite,
    /// Pondering (thinking on opponent's time)
    Ponder,
}

/// Search limits combining time control with other constraints
#[derive(Debug, Clone)]
pub struct SearchLimits {
    pub time_control: TimeControl,
    pub moves_to_go: Option<u32>, // Moves until next time control
    pub depth: Option<u32>,       // Maximum search depth
    pub nodes: Option<u64>,       // Maximum nodes to search
    pub time_parameters: Option<TimeParameters>, // Custom parameters
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            time_control: TimeControl::Infinite,
            moves_to_go: None,
            depth: None,
            nodes: None,
            time_parameters: None,
        }
    }
}

/// Time information snapshot (read-only)
#[derive(Debug, Clone)]
pub struct TimeInfo {
    pub elapsed_ms: u64,
    pub soft_limit_ms: u64,
    pub hard_limit_ms: u64,
    pub nodes_searched: u64,
    pub time_pressure: f32, // 0.0 = plenty of time, 1.0 = critical
    pub byoyomi_info: Option<ByoyomiInfo>,
}

/// Byoyomi-specific information
#[derive(Debug, Clone)]
pub struct ByoyomiInfo {
    pub in_byoyomi: bool,
    pub periods_left: u32,
    pub current_period_ms: u64,
}
