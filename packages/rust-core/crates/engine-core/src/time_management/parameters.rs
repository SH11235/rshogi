//! Tunable parameters for time management

use std::fmt;

/// Time management tunable parameters
///
/// This struct intentionally implements Copy trait for efficient passing.
/// All fields should remain primitive types to maintain Copy semantics.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(default)]
pub struct TimeParameters {
    // Overhead
    pub overhead_ms: u64,             // Default: 50
    pub network_overhead_factor: f64, // Default: 0.5 (currently unused - reserved for future network play)

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
    pub byoyomi_hard_limit_reduction_ms: u64, // Default: 500 (additional safety margin for byoyomi hard limit)

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

/// Builder for TimeParameters with validation
pub struct TimeParametersBuilder {
    params: TimeParameters,
}

/// Validation errors for time parameters
#[derive(Debug, Clone)]
pub enum TimeParameterError {
    Overhead { value: u64, min: u64, max: u64 },
    ByoyomiSafety { value: u64, min: u64, max: u64 },
    ByoyomiEarlyFinishRatio { value: u8, min: u8, max: u8 },
    PVStabilityBase { value: u64, min: u64, max: u64 },
    PVStabilitySlope { value: u64, min: u64, max: u64 },
    NetworkOverheadFactor { value: f64, min: f64, max: f64 },
    SoftMultiplier { value: f64, min: f64, max: f64 },
    HardMultiplier { value: f64, min: f64, max: f64 },
    IncrementUsage { value: f64, min: f64, max: f64 },
}

impl fmt::Display for TimeParameterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overhead { value, min, max } => {
                write!(f, "Overhead must be between {min} and {max}, got {value}")
            }
            Self::ByoyomiSafety { value, min, max } => {
                write!(f, "Byoyomi safety must be between {min} and {max}, got {value}")
            }
            Self::ByoyomiEarlyFinishRatio { value, min, max } => {
                write!(f, "Byoyomi early finish ratio must be between {min} and {max}, got {value}")
            }
            Self::PVStabilityBase { value, min, max } => {
                write!(f, "PV stability base must be between {min} and {max}, got {value}")
            }
            Self::PVStabilitySlope { value, min, max } => {
                write!(f, "PV stability slope must be between {min} and {max}, got {value}")
            }
            Self::NetworkOverheadFactor { value, min, max } => {
                write!(f, "Network overhead factor must be between {min} and {max}, got {value}")
            }
            Self::SoftMultiplier { value, min, max } => {
                write!(f, "Soft multiplier must be between {min} and {max}, got {value}")
            }
            Self::HardMultiplier { value, min, max } => {
                write!(f, "Hard multiplier must be between {min} and {max}, got {value}")
            }
            Self::IncrementUsage { value, min, max } => {
                write!(f, "Increment usage must be between {min} and {max}, got {value}")
            }
        }
    }
}

impl std::error::Error for TimeParameterError {}

/// Time management constants
pub mod constants {
    // Default values (mirrored from Default impl)
    pub const DEFAULT_OVERHEAD_MS: u64 = 50;
    pub const DEFAULT_BYOYOMI_OVERHEAD_MS: u64 = 1000; // Conservative for GUI compatibility
    pub const DEFAULT_BYOYOMI_SAFETY_MS: u64 = 500;

    // Validation ranges
    pub const MIN_OVERHEAD_MS: u64 = 0;
    pub const MAX_OVERHEAD_MS: u64 = 5000;
    pub const MIN_BYOYOMI_EARLY_FINISH_RATIO: u8 = 50;
    pub const MAX_BYOYOMI_EARLY_FINISH_RATIO: u8 = 95;
    pub const MIN_PV_STABILITY_BASE: u64 = 10;
    pub const MAX_PV_STABILITY_BASE: u64 = 200;
    pub const MIN_PV_STABILITY_SLOPE: u64 = 0;
    pub const MAX_PV_STABILITY_SLOPE: u64 = 20;
}

impl TimeParametersBuilder {
    /// Create a new builder with default values
    pub fn new() -> Self {
        Self {
            params: TimeParameters::default(),
        }
    }

    /// Set overhead in milliseconds
    pub fn overhead_ms(mut self, ms: u64) -> Result<Self, TimeParameterError> {
        if ms > constants::MAX_OVERHEAD_MS {
            return Err(TimeParameterError::Overhead {
                value: ms,
                min: constants::MIN_OVERHEAD_MS,
                max: constants::MAX_OVERHEAD_MS,
            });
        }
        self.params.overhead_ms = ms;
        Ok(self)
    }

    /// Set byoyomi safety margin (also mapped to byoyomi_hard_limit_reduction_ms)
    pub fn byoyomi_safety_ms(mut self, ms: u64) -> Result<Self, TimeParameterError> {
        if ms > 2000 {
            return Err(TimeParameterError::ByoyomiSafety {
                value: ms,
                min: 0,
                max: 2000,
            });
        }
        self.params.byoyomi_hard_limit_reduction_ms = ms;
        Ok(self)
    }

    /// Set byoyomi early finish ratio (50-95%)
    pub fn byoyomi_early_finish_ratio(mut self, ratio: u8) -> Result<Self, TimeParameterError> {
        if !(constants::MIN_BYOYOMI_EARLY_FINISH_RATIO..=constants::MAX_BYOYOMI_EARLY_FINISH_RATIO)
            .contains(&ratio)
        {
            return Err(TimeParameterError::ByoyomiEarlyFinishRatio {
                value: ratio,
                min: constants::MIN_BYOYOMI_EARLY_FINISH_RATIO,
                max: constants::MAX_BYOYOMI_EARLY_FINISH_RATIO,
            });
        }
        self.params.byoyomi_soft_ratio = (ratio as f64 / 100.0).clamp(0.5, 0.95);
        Ok(self)
    }

    /// Set PV stability base threshold
    pub fn pv_stability_base(mut self, ms: u64) -> Result<Self, TimeParameterError> {
        if !(constants::MIN_PV_STABILITY_BASE..=constants::MAX_PV_STABILITY_BASE).contains(&ms) {
            return Err(TimeParameterError::PVStabilityBase {
                value: ms,
                min: constants::MIN_PV_STABILITY_BASE,
                max: constants::MAX_PV_STABILITY_BASE,
            });
        }
        self.params.pv_base_threshold_ms = ms;
        Ok(self)
    }

    /// Set PV stability depth slope
    pub fn pv_stability_slope(mut self, ms: u64) -> Result<Self, TimeParameterError> {
        if ms > constants::MAX_PV_STABILITY_SLOPE {
            return Err(TimeParameterError::PVStabilitySlope {
                value: ms,
                min: constants::MIN_PV_STABILITY_SLOPE,
                max: constants::MAX_PV_STABILITY_SLOPE,
            });
        }
        self.params.pv_depth_slope_ms = ms;
        Ok(self)
    }

    /// Set network overhead factor (0.0 - 1.0)
    pub fn network_overhead_factor(mut self, factor: f64) -> Result<Self, TimeParameterError> {
        if !(0.0..=1.0).contains(&factor) {
            return Err(TimeParameterError::NetworkOverheadFactor {
                value: factor,
                min: 0.0,
                max: 1.0,
            });
        }
        self.params.network_overhead_factor = factor;
        Ok(self)
    }

    /// Set soft time multiplier (0.5 - 2.0)
    pub fn soft_multiplier(mut self, multiplier: f64) -> Result<Self, TimeParameterError> {
        if !(0.5..=2.0).contains(&multiplier) {
            return Err(TimeParameterError::SoftMultiplier {
                value: multiplier,
                min: 0.5,
                max: 2.0,
            });
        }
        self.params.soft_multiplier = multiplier;
        Ok(self)
    }

    /// Set hard time multiplier (2.0 - 8.0)
    pub fn hard_multiplier(mut self, multiplier: f64) -> Result<Self, TimeParameterError> {
        if !(2.0..=8.0).contains(&multiplier) {
            return Err(TimeParameterError::HardMultiplier {
                value: multiplier,
                min: 2.0,
                max: 8.0,
            });
        }
        self.params.hard_multiplier = multiplier;
        Ok(self)
    }

    /// Set increment usage factor (0.0 - 1.0)
    pub fn increment_usage(mut self, usage: f64) -> Result<Self, TimeParameterError> {
        if !(0.0..=1.0).contains(&usage) {
            return Err(TimeParameterError::IncrementUsage {
                value: usage,
                min: 0.0,
                max: 1.0,
            });
        }
        self.params.increment_usage = usage;
        Ok(self)
    }

    /// Build the final TimeParameters
    pub fn build(self) -> TimeParameters {
        self.params
    }
}

impl Default for TimeParametersBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_default_values() {
        let params = TimeParametersBuilder::new().build();
        assert_eq!(params.overhead_ms, 50);
        assert_eq!(params.byoyomi_hard_limit_reduction_ms, 500);
        assert_eq!(params.byoyomi_soft_ratio, 0.8);
        assert_eq!(params.pv_base_threshold_ms, 80);
        assert_eq!(params.pv_depth_slope_ms, 5);
    }

    #[test]
    fn test_builder_set_values() {
        let params = TimeParametersBuilder::new()
            .overhead_ms(100)
            .unwrap()
            .byoyomi_safety_ms(300)
            .unwrap()
            .byoyomi_early_finish_ratio(90)
            .unwrap()
            .pv_stability_base(120)
            .unwrap()
            .pv_stability_slope(10)
            .unwrap()
            .build();

        assert_eq!(params.overhead_ms, 100);
        assert_eq!(params.byoyomi_hard_limit_reduction_ms, 300);
        assert_eq!(params.byoyomi_soft_ratio, 0.9);
        assert_eq!(params.pv_base_threshold_ms, 120);
        assert_eq!(params.pv_depth_slope_ms, 10);
    }

    #[test]
    fn test_overhead_validation() {
        // Valid value
        assert!(TimeParametersBuilder::new().overhead_ms(1000).is_ok());

        // Too high
        let result = TimeParametersBuilder::new().overhead_ms(6000);
        assert!(result.is_err());
        match result {
            Err(TimeParameterError::Overhead { value, min, max }) => {
                assert_eq!(value, 6000);
                assert_eq!(min, constants::MIN_OVERHEAD_MS);
                assert_eq!(max, constants::MAX_OVERHEAD_MS);
            }
            _ => panic!("Expected Overhead error"),
        }
    }

    #[test]
    fn test_byoyomi_safety_validation() {
        // Valid value
        assert!(TimeParametersBuilder::new().byoyomi_safety_ms(1000).is_ok());

        // Too high
        let result = TimeParametersBuilder::new().byoyomi_safety_ms(3000);
        assert!(result.is_err());
        match result {
            Err(TimeParameterError::ByoyomiSafety { value, min, max }) => {
                assert_eq!(value, 3000);
                assert_eq!(min, 0);
                assert_eq!(max, 2000);
            }
            _ => panic!("Expected ByoyomiSafety error"),
        }
    }

    #[test]
    fn test_byoyomi_early_finish_ratio_validation() {
        // Valid values
        assert!(TimeParametersBuilder::new().byoyomi_early_finish_ratio(50).is_ok());
        assert!(TimeParametersBuilder::new().byoyomi_early_finish_ratio(80).is_ok());
        assert!(TimeParametersBuilder::new().byoyomi_early_finish_ratio(95).is_ok());

        // Too low
        let result = TimeParametersBuilder::new().byoyomi_early_finish_ratio(40);
        assert!(result.is_err());

        // Too high
        let result = TimeParametersBuilder::new().byoyomi_early_finish_ratio(100);
        assert!(result.is_err());
    }

    #[test]
    fn test_pv_stability_validation() {
        // Valid base
        assert!(TimeParametersBuilder::new().pv_stability_base(100).is_ok());

        // Base too low
        let result = TimeParametersBuilder::new().pv_stability_base(5);
        assert!(result.is_err());

        // Base too high
        let result = TimeParametersBuilder::new().pv_stability_base(300);
        assert!(result.is_err());

        // Valid slope
        assert!(TimeParametersBuilder::new().pv_stability_slope(10).is_ok());

        // Slope too high
        let result = TimeParametersBuilder::new().pv_stability_slope(30);
        assert!(result.is_err());
    }

    #[test]
    fn test_constants_match_defaults() {
        let params = TimeParameters::default();
        assert_eq!(params.overhead_ms, constants::DEFAULT_OVERHEAD_MS);
        assert_eq!(params.byoyomi_hard_limit_reduction_ms, constants::DEFAULT_BYOYOMI_SAFETY_MS);
    }

    #[test]
    fn test_error_display() {
        let err = TimeParameterError::Overhead {
            value: 10000,
            min: 0,
            max: 5000,
        };
        assert_eq!(err.to_string(), "Overhead must be between 0 and 5000, got 10000");

        let err = TimeParameterError::PVStabilityBase {
            value: 5,
            min: 10,
            max: 200,
        };
        assert_eq!(err.to_string(), "PV stability base must be between 10 and 200, got 5");
    }
}
