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

/// Canonicalize a position command for reliable storage and comparison
///
/// This function normalizes the position command string by:
/// - Removing extra whitespace
/// - Ensuring consistent token separation
/// - Preserving move order
///
/// # Arguments
/// * `startpos` - Whether this is the starting position
/// * `sfen` - Optional SFEN string (mutually exclusive with startpos)
/// * `moves` - List of moves from the position
///
/// # Returns
/// A normalized position command string
pub fn canonicalize_position_cmd(startpos: bool, sfen: Option<&str>, moves: &[String]) -> String {
    let mut cmd = String::from("position");

    if startpos {
        cmd.push_str(" startpos");
    } else if let Some(sfen_str) = sfen {
        cmd.push_str(" sfen ");
        // Normalize SFEN by trimming and collapsing multiple spaces
        let normalized_sfen = sfen_str.split_whitespace().collect::<Vec<_>>().join(" ");
        cmd.push_str(&normalized_sfen);
    }

    if !moves.is_empty() {
        cmd.push_str(" moves");
        for mv in moves {
            cmd.push(' ');
            // Trim each move to ensure no extra whitespace
            cmd.push_str(mv.trim());
        }
    }

    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonicalize_position_cmd_startpos() {
        let cmd = canonicalize_position_cmd(true, None, &[]);
        assert_eq!(cmd, "position startpos");

        let cmd = canonicalize_position_cmd(true, None, &["7g7f".to_string(), "3c3d".to_string()]);
        assert_eq!(cmd, "position startpos moves 7g7f 3c3d");
    }

    #[test]
    fn test_canonicalize_position_cmd_sfen() {
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let cmd = canonicalize_position_cmd(false, Some(sfen), &[]);
        assert_eq!(
            cmd,
            "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
        );
    }

    #[test]
    fn test_canonicalize_position_cmd_normalization() {
        // Test with extra spaces in SFEN
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL  b   -  1";
        let cmd = canonicalize_position_cmd(false, Some(sfen), &[]);
        assert_eq!(
            cmd,
            "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
        );

        // Test with moves containing extra spaces
        let moves = vec![" 7g7f ".to_string(), "3c3d  ".to_string()];
        let cmd = canonicalize_position_cmd(true, None, &moves);
        assert_eq!(cmd, "position startpos moves 7g7f 3c3d");
    }

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
