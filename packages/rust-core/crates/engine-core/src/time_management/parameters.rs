//! Tunable parameters for time management

/// Time management tunable parameters
#[derive(Debug, Clone, serde::Deserialize)]
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
            opening_factor: 1.2,
            endgame_factor: 0.8,
        }
    }
}
