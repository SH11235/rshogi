//! Time control management for the shogi engine.
//!
//! This module handles various time control modes including byoyomi, Fischer,
//! fixed time, and infinite analysis. It provides functions to infer the time
//! control mode from USI parameters and apply appropriate time limits.

use anyhow::Result;
use engine_core::search::limits::{SearchLimits, SearchLimitsBuilder};
use engine_core::shogi::{Color, Position};
use engine_core::time_management::{TimeParameters as CoreTimeParameters, TimeParametersBuilder};
use log::{debug, warn};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::usi::{clamp_periods, GoParams};

/// Helper: build TimeParameters from adapter-level overheads
fn build_time_parameters(
    overhead_ms: u32,
    byoyomi_safety_ms: u32,
    byoyomi_early_finish_ratio: u8,
    pv_base_ms: u64,
    pv_slope_ms: u64,
) -> Result<CoreTimeParameters> {
    let builder = TimeParametersBuilder::new()
        .overhead_ms(overhead_ms as u64)
        .map_err(|e| anyhow::anyhow!("Failed to set overhead_ms: {e}"))?
        .byoyomi_safety_ms(byoyomi_safety_ms as u64)
        .map_err(|e| anyhow::anyhow!("Failed to set byoyomi_safety_ms: {e}"))?
        .byoyomi_early_finish_ratio(byoyomi_early_finish_ratio)
        .map_err(|e| anyhow::anyhow!("Failed to set byoyomi_early_finish_ratio: {e}"))?
        .pv_stability_base(pv_base_ms)
        .map_err(|e| anyhow::anyhow!("Failed to set pv_stability_base: {e}"))?
        .pv_stability_slope(pv_slope_ms)
        .map_err(|e| anyhow::anyhow!("Failed to set pv_stability_slope: {e}"))?;

    Ok(builder.build())
}

/// Helper: configure builder's time control from USI params and current position
fn configure_builder_time_control(
    builder: SearchLimitsBuilder,
    params: &GoParams,
    position: &Position,
) -> SearchLimitsBuilder {
    // Fixed time per move has highest precedence among finite modes
    if let Some(movetime) = params.movetime {
        return builder.fixed_time_ms(movetime);
    }

    let btime = params.btime.unwrap_or(0);
    let wtime = params.wtime.unwrap_or(0);

    // Byoyomi branch (regular or disguised Fischer)
    if let Some(byo) = params.byoyomi {
        if byo > 0 {
            if is_fischer_disguised_as_byoyomi(byo, params.binc, params.winc) {
                // Treat as Fischer with increment
                let inc = match position.side_to_move {
                    Color::Black => params.binc.unwrap_or(byo),
                    Color::White => params.winc.unwrap_or(byo),
                };
                return builder.fischer(wtime, btime, inc);
            } else {
                // Regular byoyomi
                let periods = params.periods.map(|p| clamp_periods(p, false)).unwrap_or(1);
                let main_for_side = match position.side_to_move {
                    Color::Black => btime,
                    Color::White => wtime,
                };
                return builder.byoyomi(main_for_side, byo, periods);
            }
        }
    }

    // Fischer/Increment
    if params.binc.is_some() || params.winc.is_some() {
        let inc = match position.side_to_move {
            Color::Black => params.binc.unwrap_or(0),
            Color::White => params.winc.unwrap_or(0),
        };
        return builder.fischer(wtime, btime, inc);
    }

    // Sudden-death (btime/wtime only)
    if params.btime.is_some() || params.wtime.is_some() {
        return builder.fischer(wtime, btime, 0);
    }

    // Fallback: infinite
    builder.infinite()
}

/// Check if this is Fischer time control disguised as byoyomi
///
/// Some GUIs (like Shogidokoro) send Fischer time control in a non-standard way:
/// - byoyomi = increment value
/// - binc = winc = same increment value
/// - btime/wtime = remaining time
///
/// This pattern indicates Fischer time control where the increment is added
/// after each move, not traditional byoyomi with periods.
pub fn is_fischer_disguised_as_byoyomi(byoyomi: u64, binc: Option<u64>, winc: Option<u64>) -> bool {
    match (binc, winc) {
        (Some(b), Some(w)) => b == byoyomi && w == byoyomi,
        _ => false,
    }
}

/// Clamp search depth to prevent out-of-bounds access
///
/// USI protocol allows arbitrary depth values, but the engine has internal
/// limits to prevent array bounds violations and excessive memory usage.
pub fn clamp_depth(depth: u32) -> u8 {
    if depth > 127 {
        warn!("Requested depth {depth} exceeds maximum 127, clamping to maximum");
        127
    } else {
        depth as u8
    }
}

/// Main entry point: Apply go parameters to search limits
///
/// This function combines search limits and time control settings based on
/// the USI go command parameters.
#[allow(clippy::too_many_arguments)]
pub fn apply_go_params(
    params: &GoParams,
    position: &Position,
    overhead_ms: u32,
    stop_flag: Option<Arc<AtomicBool>>,
    byoyomi_safety_ms: u32,
    network_delay2_ms: u32,
    byoyomi_early_finish_ratio: u8,
    pv_stability_base_ms: u64,
    pv_stability_slope_ms: u64,
) -> Result<SearchLimits> {
    let mut builder = SearchLimitsBuilder::default();

    // Apply depth limit if specified
    if let Some(d) = params.depth {
        let depth_u8 = clamp_depth(d);
        builder = builder.depth(depth_u8);
        debug!("Search depth limit: {depth_u8}");
    }

    // Apply node limit if specified
    if let Some(n) = params.nodes {
        builder = builder.nodes(n);
        debug!("Search node limit: {n}");
    }

    // Apply moves to go if specified
    if let Some(mtg) = params.moves_to_go {
        builder = builder.moves_to_go(mtg);
        debug!("Moves to go: {mtg}");
    }

    // Configure time control (set inner first if pondering)
    if params.ponder {
        builder = configure_builder_time_control(builder, params, position).ponder_with_inner();
    } else {
        builder = configure_builder_time_control(builder, params, position);
    }

    // Apply time parameters (overheads and tuning mapped here)
    let mut tp = build_time_parameters(
        overhead_ms,
        byoyomi_safety_ms,
        byoyomi_early_finish_ratio,
        pv_stability_base_ms,
        pv_stability_slope_ms,
    )?;
    // Map caller-provided worst-case overhead to core param
    tp.network_delay2_ms = network_delay2_ms as u64;
    builder = builder.time_parameters(tp);

    // Apply stop flag if available
    if let Some(flag) = stop_flag {
        builder = builder.stop_flag(flag);
    }

    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_go_params() -> GoParams {
        GoParams {
            depth: None,
            nodes: None,
            movetime: None,
            infinite: false,
            ponder: false,
            btime: None,
            wtime: None,
            binc: None,
            winc: None,
            byoyomi: None,
            periods: None,
            moves_to_go: None,
        }
    }

    #[test]
    fn test_apply_go_params_depth() {
        let params = GoParams {
            depth: Some(10),
            ..make_go_params()
        };
        let position = Position::startpos();
        let limits = apply_go_params(&params, &position, 100, None, 500, 1000, 80, 80, 5).unwrap();
        assert_eq!(limits.depth, Some(10));
    }

    #[test]
    fn test_apply_go_params_nodes() {
        let params = GoParams {
            nodes: Some(1000000),
            ..make_go_params()
        };
        let position = Position::startpos();
        let limits = apply_go_params(&params, &position, 100, None, 500, 1000, 80, 80, 5).unwrap();
        assert_eq!(limits.nodes, Some(1000000));
    }

    #[test]
    fn test_apply_go_params_infinite() {
        let params = GoParams {
            infinite: true,
            ..make_go_params()
        };
        let position = Position::startpos();
        let limits = apply_go_params(&params, &position, 100, None, 500, 1000, 80, 80, 5).unwrap();
        assert!(matches!(limits.time_control, engine_core::TimeControl::Infinite));
    }

    // --- Added tests below ---

    #[test]
    fn test_apply_go_params_byoyomi_regular_with_main_time_and_periods() {
        let mut params = make_go_params();
        params.byoyomi = Some(5000);
        params.periods = Some(3);
        params.btime = Some(60000);
        params.wtime = Some(70000);

        let position = Position::startpos(); // Black to move at start
        let limits = apply_go_params(&params, &position, 50, None, 300, 1000, 80, 80, 5).unwrap();

        match limits.time_control {
            engine_core::TimeControl::Byoyomi {
                main_time_ms,
                byoyomi_ms,
                periods,
            } => {
                assert_eq!(main_time_ms, 60000); // side to move = Black
                assert_eq!(byoyomi_ms, 5000);
                assert_eq!(periods, 3);
            }
            other => panic!("Expected Byoyomi, got: {other:?}"),
        }
    }

    #[test]
    fn test_apply_go_params_byoyomi_periods_clamped() {
        use crate::usi::MAX_BYOYOMI_PERIODS;
        let mut params = make_go_params();
        params.byoyomi = Some(3000);
        params.periods = Some(100); // over max
        params.btime = Some(0);
        params.wtime = Some(0);
        let position = Position::startpos();
        let limits = apply_go_params(&params, &position, 50, None, 300, 1000, 80, 80, 5).unwrap();
        match limits.time_control {
            engine_core::TimeControl::Byoyomi { periods, .. } => {
                assert_eq!(periods, MAX_BYOYOMI_PERIODS);
            }
            _ => panic!("Expected Byoyomi"),
        }
    }

    #[test]
    fn test_apply_go_params_fischer_disguised_from_byoyomi() {
        let mut params = make_go_params();
        params.byoyomi = Some(1000);
        params.binc = Some(1000);
        params.winc = Some(1000);
        params.btime = Some(60000);
        params.wtime = Some(60000);
        let position = Position::startpos();
        let limits = apply_go_params(&params, &position, 50, None, 300, 1000, 80, 80, 5).unwrap();
        match limits.time_control {
            engine_core::TimeControl::Fischer {
                white_ms,
                black_ms,
                increment_ms,
            } => {
                assert_eq!(white_ms, 60000);
                assert_eq!(black_ms, 60000);
                assert_eq!(increment_ms, 1000);
            }
            other => panic!("Expected Fischer, got: {other:?}"),
        }
    }

    #[test]
    fn test_apply_go_params_fischer_asymmetric_increment() {
        let mut params = make_go_params();
        params.binc = Some(1000);
        params.winc = Some(2000);
        params.btime = Some(120000);
        params.wtime = Some(180000);
        let position = Position::startpos(); // Black to move -> use binc
        let limits = apply_go_params(&params, &position, 50, None, 300, 1000, 80, 80, 5).unwrap();
        match limits.time_control {
            engine_core::TimeControl::Fischer {
                white_ms,
                black_ms,
                increment_ms,
            } => {
                assert_eq!(white_ms, 180000);
                assert_eq!(black_ms, 120000);
                assert_eq!(increment_ms, 1000);
            }
            _ => panic!("Expected Fischer"),
        }
    }

    #[test]
    fn test_apply_go_params_sudden_death_maps_to_fischer_inc0() {
        let mut params = make_go_params();
        params.btime = Some(120000);
        params.wtime = Some(130000);
        let position = Position::startpos();
        let limits = apply_go_params(&params, &position, 50, None, 300, 1000, 80, 80, 5).unwrap();
        match limits.time_control {
            engine_core::TimeControl::Fischer {
                white_ms,
                black_ms,
                increment_ms,
            } => {
                assert_eq!(white_ms, 130000);
                assert_eq!(black_ms, 120000);
                assert_eq!(increment_ms, 0);
            }
            _ => panic!("Expected Fischer"),
        }
    }

    #[test]
    fn test_apply_go_params_moves_to_go_preserved() {
        let params = GoParams {
            moves_to_go: Some(20),
            ..make_go_params()
        };
        let position = Position::startpos();
        let limits = apply_go_params(&params, &position, 50, None, 300, 1000, 80, 80, 5).unwrap();
        assert_eq!(limits.moves_to_go, Some(20));
    }

    #[test]
    fn test_apply_go_params_ponder_wraps_inner_time_control() {
        let mut params = make_go_params();
        params.ponder = true;
        params.movetime = Some(1000);
        let position = Position::startpos();
        let limits = apply_go_params(&params, &position, 50, None, 300, 1000, 80, 80, 5).unwrap();

        match limits.time_control {
            engine_core::TimeControl::Ponder(inner) => match *inner {
                engine_core::TimeControl::FixedTime { ms_per_move } => {
                    assert_eq!(ms_per_move, 1000)
                }
                other => panic!("Expected inner FixedTime, got: {other:?}"),
            },
            other => panic!("Expected Ponder, got: {other:?}"),
        }
    }

    #[test]
    fn test_clamp_depth_caps_at_127() {
        let params = GoParams {
            depth: Some(200),
            ..make_go_params()
        };
        let position = Position::startpos();
        let limits = apply_go_params(&params, &position, 50, None, 300, 1000, 80, 80, 5).unwrap();
        assert_eq!(limits.depth, Some(127));
    }

    #[test]
    fn test_clamp_depth_direct() {
        assert_eq!(clamp_depth(10), 10);
        assert_eq!(clamp_depth(127), 127);
        assert_eq!(clamp_depth(128), 127);
        assert_eq!(clamp_depth(200), 127);
        assert_eq!(clamp_depth(u32::MAX), 127);
    }

    #[test]
    fn test_apply_go_params_ponder_with_disguised_fischer() {
        let mut params = make_go_params();
        params.ponder = true;
        params.byoyomi = Some(1000);
        params.binc = Some(1000);
        params.winc = Some(1000);
        params.btime = Some(60000);
        params.wtime = Some(60000);

        let position = Position::startpos();
        let limits = apply_go_params(&params, &position, 50, None, 300, 1000, 80, 80, 5).unwrap();

        match limits.time_control {
            engine_core::TimeControl::Ponder(inner) => match *inner {
                engine_core::TimeControl::Fischer {
                    white_ms,
                    black_ms,
                    increment_ms,
                } => {
                    assert_eq!(white_ms, 60000);
                    assert_eq!(black_ms, 60000);
                    assert_eq!(increment_ms, 1000);
                }
                other => panic!("Expected inner Fischer, got: {other:?}"),
            },
            other => panic!("Expected Ponder, got: {other:?}"),
        }
    }
}
