//! USI (Universal Shogi Interface) protocol implementation

pub mod commands;
pub mod conversion;
pub mod options;
pub mod output;
pub mod parser;

pub use commands::{GameResult, GoParams, UsiCommand};
pub use conversion::create_position;
pub use options::EngineOption;
pub use output::{ensure_flush_on_exit, flush_final, send_info_string, send_response, UsiResponse};
pub use parser::parse_usi_command;

/// USI option name constants
pub const OPT_BYOYOMI_PERIODS: &str = "ByoyomiPeriods";
pub const OPT_USI_BYOYOMI_PERIODS: &str = "USI_ByoyomiPeriods"; // Alias for compatibility
pub const MAX_BYOYOMI_PERIODS: u32 = 10;
pub const MIN_BYOYOMI_PERIODS: u32 = 1;

/// Time management option names
pub const OPT_OVERHEAD_MS: &str = "OverheadMs";
pub const OPT_BYOYOMI_OVERHEAD_MS: &str = "ByoyomiOverheadMs";
pub const OPT_BYOYOMI_SAFETY_MS: &str = "ByoyomiSafetyMs";

/// Clamp periods value to valid range with optional warning
pub fn clamp_periods(periods: u32, warn_on_clamp: bool) -> u32 {
    let clamped = periods.clamp(MIN_BYOYOMI_PERIODS, MAX_BYOYOMI_PERIODS);
    if warn_on_clamp && periods != clamped {
        log::warn!(
            "Periods value {periods} exceeds valid range {MIN_BYOYOMI_PERIODS}-{MAX_BYOYOMI_PERIODS}, clamping to {clamped}"
        );
    }
    clamped
}

// canonicalize_position_cmd is provided by engine-core::usi
pub use engine_core::usi::canonicalize_position_cmd;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // canonicalize_position_cmd tests moved to engine-core::usi::utils

    #[test]
    fn test_clamp_periods() {
        assert_eq!(clamp_periods(5, false), 5);
        assert_eq!(clamp_periods(0, false), MIN_BYOYOMI_PERIODS);
        assert_eq!(clamp_periods(15, false), MAX_BYOYOMI_PERIODS);

        // Test with warning enabled
        assert_eq!(clamp_periods(5, true), 5);
        assert_eq!(clamp_periods(0, true), MIN_BYOYOMI_PERIODS);
        assert_eq!(clamp_periods(15, true), MAX_BYOYOMI_PERIODS);
    }
}
