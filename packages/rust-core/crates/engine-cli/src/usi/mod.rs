//! USI (Universal Shogi Interface) protocol implementation

pub mod commands;
pub mod conversion;
pub mod options;
pub mod output;
pub mod parser;

pub use commands::{GameResult, GoParams, UsiCommand};
pub use conversion::create_position;
pub use options::EngineOption;
pub use output::{
    ensure_flush_on_exit, flush_final, send_info_string, send_response, send_response_or_exit,
    UsiResponse,
};
pub use parser::parse_usi_command;

/// USI option name constants
pub const OPT_BYOYOMI_PERIODS: &str = "ByoyomiPeriods";
pub const OPT_USI_BYOYOMI_PERIODS: &str = "USI_ByoyomiPeriods"; // Alias for compatibility
pub const MAX_BYOYOMI_PERIODS: u32 = 10;
pub const MIN_BYOYOMI_PERIODS: u32 = 1;

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

/// Standard engine options
#[allow(dead_code)]
pub fn default_options() -> Vec<EngineOption> {
    vec![
        EngineOption::spin("USI_Hash", 16, 1, 1024),
        EngineOption::check("USI_Ponder", true),
        EngineOption::spin("Threads", 1, 1, 128),
        EngineOption::button("Clear Hash"),
    ]
}
