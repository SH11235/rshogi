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
    ///
    /// Note: This represents the initial time control settings only.
    /// The actual remaining periods are tracked internally by TimeManager
    /// and exposed via TimeInfo::byoyomi_info.
    ///
    /// # Fields
    /// - `main_time_ms`: Initial main time in milliseconds
    /// - `byoyomi_ms`: Time allocated per byoyomi period
    /// - `periods`: Initial number of byoyomi periods
    Byoyomi {
        main_time_ms: u64, // Main time
        byoyomi_ms: u64,   // Time per period
        periods: u32,      // Initial number of periods (immutable)
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

/// Time state for move completion
#[derive(Debug, Clone, Copy)]
pub enum TimeState {
    /// Still in main time with remaining milliseconds
    Main { main_left_ms: u64 },
    /// In byoyomi phase with remaining main time (0 for pure byoyomi)
    Byoyomi { main_left_ms: u64 },
    /// Non-byoyomi time controls (Fischer, FixedTime, etc.)
    NonByoyomi,
}

/// Time information snapshot (read-only)
#[derive(Debug, Clone, Copy)]
pub struct TimeInfo {
    pub elapsed_ms: u64,
    pub soft_limit_ms: u64,
    pub hard_limit_ms: u64,
    pub nodes_searched: u64,
    pub time_pressure: f32, // 0.0 = plenty of time, 1.0 = critical
    pub byoyomi_info: Option<ByoyomiInfo>,
}

/// Byoyomi-specific runtime information
///
/// This represents the current state of byoyomi time control,
/// as opposed to TimeControl::Byoyomi which contains initial settings.
///
/// # Fields
/// - `in_byoyomi`: Whether currently in byoyomi (main time exhausted)
/// - `periods_left`: Number of byoyomi periods remaining
/// - `current_period_ms`: Time remaining in current period
#[derive(Debug, Clone, Copy)]
pub struct ByoyomiInfo {
    pub in_byoyomi: bool,
    pub periods_left: u32,
    pub current_period_ms: u64,
}
