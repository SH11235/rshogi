//! Tunable parameters for time management

/// Time management tunable parameters
///
/// This struct intentionally implements Copy trait for efficient passing.
/// All fields should remain primitive types to maintain Copy semantics.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(default)]
pub struct TimeParameters {
    // Overhead
    pub overhead_ms: u64,             // Default: 50
    pub network_overhead_factor: f64, // Default: 0.5

    // PV stability
    pub pv_base_threshold_ms: u64, // Default: 80
    pub pv_depth_slope_ms: u64,    // Default: 5

    // Critical time thresholds
    pub critical_fischer_ms: u64, // Default: 300
    pub critical_byoyomi_ms: u64, // Default: 80

    // Time allocation multipliers
    pub soft_multiplier: f64, // Default: 1.0
    pub hard_multiplier: f64, // Default: 4.0
    pub increment_usage: f64, // Default: 0.8

    // Byoyomi specific
    pub byoyomi_soft_ratio: f64, // Default: 0.8 (80% of byoyomi time)
    pub byoyomi_hard_limit_reduction_ms: u64, // Default: 300 (additional safety margin for byoyomi hard limit)

    // Game phase factors
    pub opening_factor: f64, // Default: 1.2
    pub endgame_factor: f64, // Default: 0.8
}

impl Default for TimeParameters {
    fn default() -> Self {
        Self {
            overhead_ms: 50,
            network_overhead_factor: 0.5,
            pv_base_threshold_ms: 80,
            pv_depth_slope_ms: 5,
            critical_fischer_ms: 300,
            critical_byoyomi_ms: 80,
            soft_multiplier: 1.0,
            hard_multiplier: 4.0,
            increment_usage: 0.8,
            byoyomi_soft_ratio: 0.8,
            byoyomi_hard_limit_reduction_ms: 500,
            opening_factor: 1.2,
            endgame_factor: 0.8,
        }
    }
}
