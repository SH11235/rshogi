//! Time control utilities for the USI engine
//!
//! This module provides functionality for inferring and applying various time control
//! modes from USI go command parameters, including support for:
//! - Ponder mode
//! - Infinite analysis
//! - Fixed time per move
//! - Byoyomi (Japanese time control)
//! - Fischer increment
//! - Default time control

use anyhow::Result;
use engine_core::{
    search::constants::MAX_PLY,
    search::limits::{SearchLimits, SearchLimitsBuilder},
    shogi::Position,
};

use crate::usi::GoParams;

/// Inferred time control mode from go parameters
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeControlMode {
    /// Ponder mode - thinking on opponent's time
    Ponder,
    /// Infinite analysis mode
    Infinite,
    /// Fixed time per move
    FixedTime(u64),
    /// Byoyomi time control (Japanese style)
    Byoyomi,
    /// Fischer time control (with increment)
    Fischer,
    /// Default time control when none specified
    Default,
}

/// Infer time control mode from go parameters
///
/// Priority: ponder > infinite > movetime > byoyomi > fischer > default
pub fn infer_time_control_mode(params: &GoParams) -> TimeControlMode {
    if params.ponder {
        TimeControlMode::Ponder
    } else if params.infinite {
        TimeControlMode::Infinite
    } else if let Some(movetime) = params.movetime {
        TimeControlMode::FixedTime(movetime)
    } else if params.byoyomi.is_some() {
        // Check if this is actually Fischer time control disguised as byoyomi
        if is_fischer_disguised_as_byoyomi(params) {
            TimeControlMode::Fischer
        } else {
            TimeControlMode::Byoyomi
        }
    } else if params.periods.is_some() {
        // If periods is specified without byoyomi, it's still Byoyomi mode
        log::debug!("Periods specified without byoyomi, using Byoyomi mode with byoyomi=0");
        TimeControlMode::Byoyomi
    } else if params.btime.is_some() || params.wtime.is_some() {
        TimeControlMode::Fischer
    } else {
        TimeControlMode::Default
    }
}

/// Check if the go parameters represent Fischer time control disguised as byoyomi
///
/// Some GUIs send byoyomi=0 with binc/winc for Fischer time control
/// However, if periods is specified, it's definitely Byoyomi
pub fn is_fischer_disguised_as_byoyomi(params: &GoParams) -> bool {
    params.byoyomi == Some(0)
        && (params.binc.is_some() || params.winc.is_some())
        && params.periods.is_none()
}

/// Pick appropriate overhead milliseconds based on effective time control mode
///
/// This function determines the appropriate overhead value to use based on the
/// time control mode. Byoyomi mode typically requires different overhead handling
/// than other time controls due to its strict per-move time limits.
pub fn pick_overhead_for(params: &GoParams, overhead_ms: u64, byoyomi_overhead_ms: u64) -> u64 {
    match infer_time_control_mode(params) {
        TimeControlMode::Ponder => {
            // For ponder, determine the inner mode by removing ponder flag
            let inner = GoParams {
                ponder: false,
                ..params.clone()
            };
            match infer_time_control_mode(&inner) {
                TimeControlMode::Byoyomi => byoyomi_overhead_ms,
                _ => overhead_ms,
            }
        }
        TimeControlMode::Byoyomi => byoyomi_overhead_ms,
        _ => overhead_ms,
    }
}

/// Apply time control based on inferred mode
pub fn apply_time_control(
    builder: SearchLimitsBuilder,
    params: &GoParams,
    position: &Position,
    byoyomi_periods: u32,
) -> SearchLimitsBuilder {
    let mode = infer_time_control_mode(params);

    match mode {
        TimeControlMode::Ponder => {
            // For ponder, we need to determine what the actual time control would be
            // if ponder was false, then use that information
            let non_ponder_mode = {
                let temp_params = GoParams {
                    ponder: false,
                    ..params.clone()
                };
                infer_time_control_mode(&temp_params)
            };

            // Apply the actual time control that will be used after ponder hit
            let builder_with_time = match non_ponder_mode {
                TimeControlMode::Infinite => apply_infinite_mode(builder),
                TimeControlMode::FixedTime(movetime) => apply_fixed_time_mode(builder, movetime),
                TimeControlMode::Byoyomi => {
                    apply_byoyomi_mode(builder, params, position, byoyomi_periods)
                }
                TimeControlMode::Fischer => apply_fischer_mode(builder, params, position),
                TimeControlMode::Default => apply_default_time_control(builder),
                TimeControlMode::Ponder => unreachable!("Ponder mode should not be nested"),
            };

            // Now override with ponder mode while preserving time information
            // This ensures the SearchLimits has the correct time information
            // wrapped inside TimeControl::Ponder
            builder_with_time.ponder_with_inner()
        }
        TimeControlMode::Infinite => apply_infinite_mode(builder),
        TimeControlMode::FixedTime(movetime) => apply_fixed_time_mode(builder, movetime),
        TimeControlMode::Byoyomi => apply_byoyomi_mode(builder, params, position, byoyomi_periods),
        TimeControlMode::Fischer => apply_fischer_mode(builder, params, position),
        TimeControlMode::Default => apply_default_time_control(builder),
    }
}

/// Apply infinite mode time control
pub fn apply_infinite_mode(builder: SearchLimitsBuilder) -> SearchLimitsBuilder {
    builder.infinite()
}

/// Apply fixed time mode with specified time per move
pub fn apply_fixed_time_mode(builder: SearchLimitsBuilder, movetime: u64) -> SearchLimitsBuilder {
    builder.fixed_time_ms(movetime)
}

/// Apply byoyomi time control
pub fn apply_byoyomi_mode(
    builder: SearchLimitsBuilder,
    params: &GoParams,
    position: &Position,
    byoyomi_periods: u32,
) -> SearchLimitsBuilder {
    let byoyomi = params.byoyomi.unwrap_or(0);
    // When btime/wtime is not specified, it means main_time = 0 (pure byoyomi)
    let main_time = match position.side_to_move {
        engine_core::shogi::Color::Black => params.btime.unwrap_or(0),
        engine_core::shogi::Color::White => params.wtime.unwrap_or(0),
    };
    builder.byoyomi(main_time, byoyomi, byoyomi_periods)
}

/// Apply Fischer time control
pub fn apply_fischer_mode(
    builder: SearchLimitsBuilder,
    params: &GoParams,
    position: &Position,
) -> SearchLimitsBuilder {
    let black_time = params.btime.unwrap_or(0);
    let white_time = params.wtime.unwrap_or(0);

    // Handle asymmetric increments
    let incr_b = params.binc.unwrap_or(0);
    let incr_w = params.winc.unwrap_or(0);

    // Since SearchLimits builder only supports symmetric increment,
    // use the side-to-move's increment for accurate time management
    let increment = match position.side_to_move {
        engine_core::shogi::Color::Black => incr_b,
        engine_core::shogi::Color::White => incr_w,
    };

    if incr_b != incr_w {
        log::debug!(
            "Asymmetric Fischer increments (binc={incr_b}, winc={incr_w}), using {increment} for {:?} side",
            position.side_to_move
        );
    }

    // TODO: When SearchLimits supports asymmetric increments, update to:
    // builder.fischer_asymmetric(black_time, white_time, incr_b, incr_w)
    builder.fischer(black_time, white_time, increment)
}

/// Apply default time control when no specific time control is specified
pub fn apply_default_time_control(builder: SearchLimitsBuilder) -> SearchLimitsBuilder {
    log::warn!("No time control specified in go command, defaulting to 5 seconds");
    builder.fixed_time_ms(5000)
}

/// Validate and clamp search depth to ensure it's within valid range
pub fn validate_and_clamp_depth(depth: u32) -> u8 {
    // Ensure minimum depth of 1 to prevent "no search" scenario
    let safe_depth = if depth == 0 {
        log::warn!("Depth 0 is not supported, using minimum depth of 1");
        1
    } else {
        depth
    };

    // Clamp depth to MAX_PLY to prevent array bounds violation
    let clamped_depth = safe_depth.min(MAX_PLY as u32);
    if safe_depth != clamped_depth {
        log::warn!(
            "Depth {safe_depth} exceeds maximum supported depth {MAX_PLY}, clamping to {clamped_depth}"
        );
    }
    clamped_depth as u8
}

/// Apply search limits (depth, nodes, moves_to_go) from go parameters
pub fn apply_search_limits(
    mut builder: SearchLimitsBuilder,
    params: &GoParams,
) -> SearchLimitsBuilder {
    // Set depth limit
    if let Some(depth) = params.depth {
        let clamped_depth = validate_and_clamp_depth(depth);
        builder = builder.depth(clamped_depth);
    }

    // Set node limit
    if let Some(nodes) = params.nodes {
        builder = builder.nodes(nodes);
    }

    // Set moves to go
    if let Some(moves_to_go) = params.moves_to_go {
        builder = builder.moves_to_go(moves_to_go);
    }

    builder
}

/// Convert USI go parameters to search limits with proper time control
///
/// Priority order: ponder > infinite > movetime > byoyomi > fischer > default
pub fn apply_go_params(
    builder: SearchLimitsBuilder,
    params: &GoParams,
    position: &Position,
    byoyomi_periods: u32,
) -> Result<SearchLimits> {
    let builder = apply_search_limits(builder, params);
    // Use periods from go command if specified, otherwise use the provided default
    let effective_periods = params.periods.unwrap_or(byoyomi_periods);

    let builder = apply_time_control(builder, params, position, effective_periods);
    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::time_management::TimeControl;

    #[test]
    fn test_infer_time_control_mode_priority() {
        // Test ponder has highest priority
        let params = GoParams {
            ponder: true,
            infinite: true,
            movetime: Some(1000),
            byoyomi: Some(5000),
            btime: Some(60000),
            ..Default::default()
        };
        assert_eq!(infer_time_control_mode(&params), TimeControlMode::Ponder);

        // Test infinite has second priority
        let params = GoParams {
            infinite: true,
            movetime: Some(1000),
            byoyomi: Some(5000),
            ..Default::default()
        };
        assert_eq!(infer_time_control_mode(&params), TimeControlMode::Infinite);

        // Test movetime has third priority
        let params = GoParams {
            movetime: Some(1000),
            byoyomi: Some(5000),
            ..Default::default()
        };
        assert_eq!(infer_time_control_mode(&params), TimeControlMode::FixedTime(1000));

        // Test byoyomi has fourth priority
        let params = GoParams {
            byoyomi: Some(5000),
            btime: Some(60000),
            ..Default::default()
        };
        assert_eq!(infer_time_control_mode(&params), TimeControlMode::Byoyomi);

        // Test fischer
        let params = GoParams {
            btime: Some(60000),
            wtime: Some(60000),
            ..Default::default()
        };
        assert_eq!(infer_time_control_mode(&params), TimeControlMode::Fischer);

        // Test default
        let params = GoParams::default();
        assert_eq!(infer_time_control_mode(&params), TimeControlMode::Default);
    }

    #[test]
    fn test_is_fischer_disguised_as_byoyomi() {
        // Test genuine Fischer disguised as byoyomi
        let params = GoParams {
            byoyomi: Some(0),
            binc: Some(1000),
            winc: Some(1000),
            ..Default::default()
        };
        assert!(is_fischer_disguised_as_byoyomi(&params));

        // Test genuine byoyomi (with periods)
        let params = GoParams {
            byoyomi: Some(0),
            binc: Some(1000),
            periods: Some(1),
            ..Default::default()
        };
        assert!(!is_fischer_disguised_as_byoyomi(&params));

        // Test genuine byoyomi (non-zero byoyomi)
        let params = GoParams {
            byoyomi: Some(5000),
            binc: Some(1000),
            ..Default::default()
        };
        assert!(!is_fischer_disguised_as_byoyomi(&params));

        // Test no increment
        let params = GoParams {
            byoyomi: Some(0),
            ..Default::default()
        };
        assert!(!is_fischer_disguised_as_byoyomi(&params));
    }

    #[test]
    fn test_pick_overhead_for() {
        let overhead_ms = 100;
        let byoyomi_overhead_ms = 50;

        // Test byoyomi mode
        let params = GoParams {
            byoyomi: Some(5000),
            ..Default::default()
        };
        assert_eq!(
            pick_overhead_for(&params, overhead_ms, byoyomi_overhead_ms),
            byoyomi_overhead_ms
        );

        // Test fischer mode
        let params = GoParams {
            btime: Some(60000),
            ..Default::default()
        };
        assert_eq!(pick_overhead_for(&params, overhead_ms, byoyomi_overhead_ms), overhead_ms);

        // Test ponder with byoyomi inner
        let params = GoParams {
            ponder: true,
            byoyomi: Some(5000),
            ..Default::default()
        };
        assert_eq!(
            pick_overhead_for(&params, overhead_ms, byoyomi_overhead_ms),
            byoyomi_overhead_ms
        );

        // Test ponder with fischer inner
        let params = GoParams {
            ponder: true,
            btime: Some(60000),
            ..Default::default()
        };
        assert_eq!(pick_overhead_for(&params, overhead_ms, byoyomi_overhead_ms), overhead_ms);
    }

    #[test]
    fn test_validate_and_clamp_depth() {
        // Test zero depth clamping
        assert_eq!(validate_and_clamp_depth(0), 1);

        // Test normal depth
        assert_eq!(validate_and_clamp_depth(10), 10);

        // Test maximum depth
        assert_eq!(validate_and_clamp_depth(MAX_PLY as u32), MAX_PLY as u8);

        // Test exceeding maximum depth
        assert_eq!(validate_and_clamp_depth(MAX_PLY as u32 + 10), MAX_PLY as u8);
    }

    #[test]
    fn test_apply_search_limits() {
        let builder = SearchLimits::builder();

        // Test with all parameters
        let params = GoParams {
            depth: Some(20),
            nodes: Some(1000000),
            moves_to_go: Some(40),
            ..Default::default()
        };

        let limits = apply_search_limits(builder, &params).build();
        assert_eq!(limits.depth, Some(20));
        assert_eq!(limits.nodes, Some(1000000));
        assert_eq!(limits.moves_to_go, Some(40));
    }

    #[test]
    fn test_apply_go_params() {
        let position = Position::startpos();
        let builder = SearchLimits::builder();

        // Test byoyomi mode
        let params = GoParams {
            byoyomi: Some(5000),
            btime: Some(60000),
            periods: Some(3),
            ..Default::default()
        };

        let limits = apply_go_params(builder, &params, &position, 1).unwrap();
        match limits.time_control {
            TimeControl::Byoyomi { .. } => {}
            _ => panic!("Expected Byoyomi time control"),
        }
    }
}
