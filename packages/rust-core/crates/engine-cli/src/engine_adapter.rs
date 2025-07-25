//! Engine adapter for USI protocol
//!
//! This module bridges the USI protocol with the engine-core implementation,
//! handling position management, search parameter conversion, and result formatting.

use anyhow::{anyhow, Result};
use engine_core::{
    engine::controller::{Engine, EngineType},
    search::constants::{MATE_SCORE, MAX_PLY},
    search::limits::{SearchLimits, SearchLimitsBuilder},
    shogi::Position,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::usi::output::{Score, SearchInfo};
use crate::usi::{create_position, EngineOption, GameResult, GoParams};
use crate::usi::{MAX_BYOYOMI_PERIODS, MIN_BYOYOMI_PERIODS, OPT_BYOYOMI_PERIODS};

/// Convert raw engine score to USI score format (Cp or Mate)
fn to_usi_score(raw_score: i32) -> Score {
    if raw_score.abs() >= MATE_SCORE - MAX_PLY as i32 {
        // It's a mate score - calculate mate distance
        let mate_in_half = MATE_SCORE - raw_score.abs();
        // Calculate mate in moves (1 move = 2 plies)
        // Use max(1) to avoid "mate 0" (some GUIs prefer "mate 1" for immediate mate)
        let mate_in = ((mate_in_half + 1) / 2).max(1);
        if raw_score > 0 {
            Score::Mate(mate_in)
        } else {
            Score::Mate(-mate_in)
        }
    } else {
        Score::Cp(raw_score)
    }
}

/// Engine adapter that bridges USI protocol with engine-core
pub struct EngineAdapter {
    /// The underlying engine (wrapped in Option for temporary ownership transfer)
    engine: Option<Engine>,
    /// Current position
    position: Option<Position>,
    /// Engine options
    options: Vec<EngineOption>,
    /// Hash table size in MB
    hash_size: usize,
    /// Number of threads
    threads: usize,
    /// Enable pondering
    ponder: bool,
    /// Byoyomi periods (default: 1)
    byoyomi_periods: u32,
    /// Ponder state for managing ponder searches
    ponder_state: PonderState,
    /// Active ponder hit flag (shared with searcher during ponder)
    active_ponder_hit_flag: Option<Arc<AtomicBool>>,
}

/// State for managing ponder (think on opponent's time) functionality
#[derive(Debug, Clone, Default)]
struct PonderState {
    /// Whether currently pondering
    is_pondering: bool,
    /// The move we're pondering on (opponent's expected move)
    ponder_move: Option<String>,
    /// Time when pondering started
    ponder_start_time: Option<std::time::Instant>,
}

impl Default for EngineAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineAdapter {
    /// Take the engine out for exclusive use (for search operations)
    pub fn take_engine(&mut self) -> Result<Engine> {
        self.engine
            .take()
            .ok_or_else(|| anyhow!("Engine is currently in use (probably searching)"))
    }

    /// Return the engine after use
    pub fn return_engine(&mut self, engine: Engine) {
        if self.engine.is_some() {
            log::warn!("Engine returned while another engine instance exists");
        }
        self.engine = Some(engine);
    }

    /// Create a new engine adapter
    pub fn new() -> Self {
        let mut adapter = Self {
            engine: Some(Engine::new(EngineType::Material)), // Start with simplest for compatibility
            position: None,
            options: Vec::new(),
            hash_size: 16,
            threads: 1,
            ponder: true,
            byoyomi_periods: 1,
            ponder_state: PonderState::default(),
            active_ponder_hit_flag: None,
        };

        // Initialize options
        adapter.init_options();
        adapter
    }

    /// Initialize engine options
    fn init_options(&mut self) {
        self.options = vec![
            EngineOption::spin("USI_Hash", 16, 1, 1024),
            EngineOption::spin("Threads", 1, 1, 256),
            EngineOption::check("USI_Ponder", true),
            EngineOption::combo(
                "EngineType",
                "Material".to_string(),
                vec![
                    "Material".to_string(),
                    "Nnue".to_string(),
                    "Enhanced".to_string(),
                    "EnhancedNnue".to_string(),
                ],
            ),
            EngineOption::spin(
                OPT_BYOYOMI_PERIODS,
                1,
                MIN_BYOYOMI_PERIODS as i64,
                MAX_BYOYOMI_PERIODS as i64,
            ),
        ];
    }

    /// Get available options
    pub fn get_options(&self) -> &[EngineOption] {
        &self.options
    }

    /// Initialize the engine
    pub fn initialize(&mut self) -> Result<()> {
        // Engine is ready
        Ok(())
    }

    /// Set position from USI command
    pub fn set_position(
        &mut self,
        startpos: bool,
        sfen: Option<&str>,
        moves: &[String],
    ) -> Result<()> {
        self.position = Some(create_position(startpos, sfen, moves)?);

        // Clear ponder state when position changes
        self.clear_ponder_state();

        Ok(())
    }

    /// Set engine option
    pub fn set_option(&mut self, name: &str, value: Option<&str>) -> Result<()> {
        match name {
            "USI_Hash" => {
                if let Some(val) = value {
                    self.hash_size = val.parse::<usize>().map_err(|_| {
                        anyhow!("Invalid hash size: '{}'. Must be a number between 1 and 1024", val)
                    })?;
                }
            }
            "Threads" => {
                if let Some(val) = value {
                    self.threads = val.parse::<usize>().map_err(|_| {
                        anyhow!(
                            "Invalid thread count: '{}'. Must be a number between 1 and 256",
                            val
                        )
                    })?;
                }
            }
            "USI_Ponder" => {
                if let Some(val) = value {
                    self.ponder = val.to_lowercase() == "true";
                }
            }
            "EngineType" => {
                if let Some(val) = value {
                    let engine_type = match val {
                        "Material" => EngineType::Material,
                        "Nnue" => EngineType::Nnue,
                        "Enhanced" => EngineType::Enhanced,
                        "EnhancedNnue" => EngineType::EnhancedNnue,
                        _ => return Err(anyhow!("Invalid engine type: '{}'. Valid values are: Material, Nnue, Enhanced, EnhancedNnue", val)),
                    };
                    if let Some(ref mut engine) = self.engine {
                        engine.set_engine_type(engine_type);
                    } else {
                        return Err(anyhow!("Engine is currently in use"));
                    }
                }
            }
            OPT_BYOYOMI_PERIODS => {
                if let Some(val) = value {
                    self.byoyomi_periods = val
                        .parse::<u32>()
                        .map_err(|_| {
                            anyhow!(
                                "Invalid {}: '{}'. Must be a number between {} and {}",
                                OPT_BYOYOMI_PERIODS,
                                val,
                                MIN_BYOYOMI_PERIODS,
                                MAX_BYOYOMI_PERIODS
                            )
                        })?
                        .clamp(MIN_BYOYOMI_PERIODS, MAX_BYOYOMI_PERIODS);
                }
            }
            _ => {
                return Err(anyhow!("Unknown option: '{}'. Available options: USI_Hash, Threads, USI_Ponder, EngineType, {}", name, OPT_BYOYOMI_PERIODS));
            }
        }
        Ok(())
    }

    /// Handle ponder hit (opponent played the expected move)
    pub fn ponder_hit(&mut self) -> Result<()> {
        if let Some(ref flag) = self.active_ponder_hit_flag {
            flag.store(true, Ordering::Release);

            // Clear ponder state since we're transitioning to normal search
            self.ponder_state.is_pondering = false;

            log::info!("Ponder hit: Converting ponder search to normal search");
            Ok(())
        } else {
            Err(anyhow!("Ponder hit received but engine is not in ponder mode"))
        }
    }

    /// Handle game over
    pub fn game_over(&mut self, _result: GameResult) {
        // Clear position and prepare for new game
        self.position = None;

        // Clear ponder state
        self.clear_ponder_state();
    }

    /// Clear ponder state
    fn clear_ponder_state(&mut self) {
        self.ponder_state.is_pondering = false;
        self.ponder_state.ponder_move = None;
        self.ponder_state.ponder_start_time = None;
        self.active_ponder_hit_flag = None;
    }

    /// Prepare search data and return necessary components
    /// This allows releasing the mutex lock before the actual search
    pub fn prepare_search(
        &mut self,
        params: &GoParams,
        stop_flag: Arc<AtomicBool>,
    ) -> Result<(Position, SearchLimits, Option<Arc<AtomicBool>>)> {
        log::info!(
            "Starting {} search - depth:{:?} time:{:?}ms nodes:{:?}",
            if params.ponder { "ponder" } else { "normal" },
            params.depth,
            params.movetime.or(params.btime.or(params.wtime)),
            params.nodes
        );

        // Get position
        let position = self.position.clone().ok_or_else(|| {
            anyhow!("Position not set. Use 'position startpos' or 'position sfen ...' first")
        })?;

        // Update ponder state and create flag if needed
        let ponder_hit_flag = if params.ponder {
            self.ponder_state.is_pondering = true;
            self.ponder_state.ponder_start_time = Some(std::time::Instant::now());
            let flag = Arc::new(AtomicBool::new(false));
            self.active_ponder_hit_flag = Some(flag.clone());
            log::debug!("Ponder mode activated with shared flag");
            Some(flag)
        } else {
            self.clear_ponder_state();
            None
        };

        // Create SearchLimits
        let mut builder = SearchLimits::builder();

        // Set stop flag
        builder = builder.stop_flag(stop_flag);

        // Set ponder hit flag if available
        if let Some(ref flag) = ponder_hit_flag {
            builder = builder.ponder_hit_flag(flag.clone());
        }

        // Apply go parameters
        // Use periods from go command if specified, otherwise use SetOption value
        let periods = params.periods.unwrap_or(self.byoyomi_periods);
        let limits = apply_go_params(builder, params, &position, periods)?;

        Ok((position, limits, ponder_hit_flag))
    }

    /// Execute search with prepared data and return USI result
    /// This takes a mutable reference to avoid ownership transfer
    pub fn execute_search_static(
        engine: &mut Engine,
        mut position: Position,
        limits: SearchLimits,
        info_callback: Box<dyn Fn(SearchInfo) + Send + Sync>,
    ) -> Result<(String, Option<String>)> {
        // Set up info callback
        let info_callback_arc = Arc::new(info_callback);
        let info_callback_clone = info_callback_arc.clone();
        type InfoCallbackType = Arc<
            dyn Fn(u8, i32, u64, std::time::Duration, &[engine_core::shogi::Move]) + Send + Sync,
        >;
        let info_callback_inner: InfoCallbackType =
            Arc::new(move |depth, score, nodes, elapsed, pv| {
                let pv_str: Vec<String> = pv.iter().map(engine_core::usi::move_to_usi).collect();
                let score_enum = to_usi_score(score);

                let info = SearchInfo {
                    depth: Some(depth as u32),
                    time: Some(elapsed.as_millis().max(1) as u64),
                    nodes: Some(nodes),
                    pv: pv_str,
                    score: Some(score_enum),
                    ..Default::default()
                };
                (*info_callback_clone)(info);
            });

        // Create a new SearchLimits with info_callback added
        // We can't add it in prepare_search because the callback is created here
        // This is not shadowing - we're creating a new instance with the callback
        let limits = SearchLimits {
            info_callback: Some(info_callback_inner),
            ..limits
        };

        // Execute search
        let result = engine.search(&mut position, limits);
        log::info!(
            "Search completed - depth:{} nodes:{} time:{}ms",
            result.stats.depth,
            result.stats.nodes,
            result.stats.elapsed.as_millis()
        );

        // Get best move
        let best_move = result.best_move.ok_or_else(|| {
            anyhow!("No legal moves available in this position (checkmate or stalemate)")
        })?;

        // Convert move to USI format
        let best_move_str = engine_core::usi::move_to_usi(&best_move);
        log::info!("Best move: {} (score: {:?})", best_move_str, result.score);

        // Send final info
        let score = to_usi_score(result.score);
        let depth = if result.stats.depth == 0 {
            log::warn!("SearchStats.depth is 0, this should not happen. Using 1 as fallback.");
            1
        } else {
            result.stats.depth
        };

        let info = SearchInfo {
            depth: Some(depth as u32),
            time: Some(result.stats.elapsed.as_millis().max(1) as u64),
            nodes: Some(result.stats.nodes),
            pv: vec![best_move_str.clone()],
            score: Some(score),
            ..Default::default()
        };
        (*info_callback_arc)(info);

        // For now, no ponder move generation
        Ok((best_move_str, None))
    }

    /// Clean up after search completion
    pub fn cleanup_after_search(&mut self, was_ponder: bool) {
        if was_ponder {
            self.active_ponder_hit_flag = None;
            if self.ponder_state.is_pondering {
                self.ponder_state.is_pondering = false;
            }
        }
    }
}

/// Validate and clamp search depth to ensure it's within valid range
fn validate_and_clamp_depth(depth: u32) -> u32 {
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
    clamped_depth
}

/// Check if the go parameters represent Fischer time control disguised as byoyomi
///
/// Some GUIs send byoyomi=0 with binc/winc for Fischer time control
/// However, if periods is specified, it's definitely Byoyomi
fn is_fischer_disguised_as_byoyomi(params: &GoParams) -> bool {
    params.byoyomi == Some(0)
        && (params.binc.is_some() || params.winc.is_some())
        && params.periods.is_none()
}

/// Get the increment for the given side from go parameters
fn get_increment_for_side(params: &GoParams, side: engine_core::shogi::Color) -> u64 {
    match side {
        engine_core::shogi::Color::Black => params.binc.unwrap_or(0),
        engine_core::shogi::Color::White => params.winc.unwrap_or(0),
    }
}

/// Apply ponder mode time control
fn apply_ponder_mode(builder: SearchLimitsBuilder) -> SearchLimitsBuilder {
    builder.ponder()
}

/// Apply infinite mode time control
fn apply_infinite_mode(builder: SearchLimitsBuilder) -> SearchLimitsBuilder {
    builder.infinite()
}

/// Apply fixed time mode with specified time per move
fn apply_fixed_time_mode(builder: SearchLimitsBuilder, movetime: u64) -> SearchLimitsBuilder {
    builder.fixed_time_ms(movetime)
}

/// Apply byoyomi time control
fn apply_byoyomi_mode(
    builder: SearchLimitsBuilder,
    params: &GoParams,
    position: &Position,
    byoyomi_periods: u32,
) -> SearchLimitsBuilder {
    let main_time = match position.side_to_move {
        engine_core::shogi::Color::Black => params.btime.unwrap_or(0),
        engine_core::shogi::Color::White => params.wtime.unwrap_or(0),
    };
    let byoyomi = params.byoyomi.unwrap_or(0);
    builder.byoyomi(main_time, byoyomi, byoyomi_periods)
}

/// Apply Fischer time control
fn apply_fischer_mode(
    builder: SearchLimitsBuilder,
    params: &GoParams,
    position: &Position,
) -> SearchLimitsBuilder {
    let black_time = params.btime.unwrap_or(0);
    let white_time = params.wtime.unwrap_or(0);
    let increment = get_increment_for_side(params, position.side_to_move);

    // Note: fischer() expects (black_ms, white_ms, increment_ms)
    builder.fischer(black_time, white_time, increment)
}

/// Apply default time control when no specific time control is specified
fn apply_default_time_control(builder: SearchLimitsBuilder) -> SearchLimitsBuilder {
    log::warn!("No time control specified in go command, defaulting to 5 seconds");
    builder.fixed_time_ms(5000)
}

/// Apply search limits (depth, nodes, moves_to_go) from go parameters
fn apply_search_limits(mut builder: SearchLimitsBuilder, params: &GoParams) -> SearchLimitsBuilder {
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

/// Inferred time control mode from go parameters
enum TimeControlMode {
    Ponder,
    Infinite,
    FixedTime(u64),
    Byoyomi,
    Fischer,
    Default,
}

/// Infer time control mode from go parameters
///
/// Priority: ponder > infinite > movetime > byoyomi > fischer > default
fn infer_time_control_mode(params: &GoParams) -> TimeControlMode {
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
        log::info!("Periods specified without byoyomi, using Byoyomi mode with byoyomi=0");
        TimeControlMode::Byoyomi
    } else if params.btime.is_some() || params.wtime.is_some() {
        TimeControlMode::Fischer
    } else {
        TimeControlMode::Default
    }
}

/// Apply time control based on inferred mode
fn apply_time_control(
    builder: SearchLimitsBuilder,
    params: &GoParams,
    position: &Position,
    byoyomi_periods: u32,
) -> SearchLimitsBuilder {
    let mode = infer_time_control_mode(params);

    match mode {
        TimeControlMode::Ponder => apply_ponder_mode(builder),
        TimeControlMode::Infinite => apply_infinite_mode(builder),
        TimeControlMode::FixedTime(movetime) => apply_fixed_time_mode(builder, movetime),
        TimeControlMode::Byoyomi => apply_byoyomi_mode(builder, params, position, byoyomi_periods),
        TimeControlMode::Fischer => apply_fischer_mode(builder, params, position),
        TimeControlMode::Default => apply_default_time_control(builder),
    }
}

/// Convert USI go parameters to search limits with proper time control
///
/// Priority order: ponder > infinite > movetime > byoyomi > fischer > default
fn apply_go_params(
    builder: SearchLimitsBuilder,
    params: &GoParams,
    position: &Position,
    byoyomi_periods: u32,
) -> Result<SearchLimits> {
    let builder = apply_search_limits(builder, params);
    // Use periods from go command if specified, otherwise use the provided default
    let periods = params.periods.unwrap_or(byoyomi_periods);
    let builder = apply_time_control(builder, params, position, periods);
    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::shogi::Position;
    use engine_core::time_management::TimeControl;

    const DEFAULT_BYOYOMI_PERIODS: u32 = 1;

    fn create_test_position() -> Position {
        Position::startpos()
    }

    #[test]
    fn test_apply_go_params_ponder() {
        let params = GoParams {
            ponder: true,
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        match limits.time_control {
            TimeControl::Ponder => {}
            _ => panic!("Expected Ponder time control"),
        }
    }

    #[test]
    fn test_apply_go_params_infinite() {
        let params = GoParams {
            infinite: true,
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        match limits.time_control {
            TimeControl::Infinite => {}
            _ => panic!("Expected Infinite time control"),
        }
    }

    #[test]
    fn test_apply_go_params_movetime() {
        let params = GoParams {
            movetime: Some(1000),
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        match limits.time_control {
            TimeControl::FixedTime { ms_per_move } => {
                assert_eq!(ms_per_move, 1000);
            }
            _ => panic!("Expected FixedTime time control"),
        }
    }

    #[test]
    fn test_apply_go_params_byoyomi() {
        let params = GoParams {
            byoyomi: Some(30000),
            btime: Some(600000),
            wtime: Some(600000),
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        match limits.time_control {
            TimeControl::Byoyomi {
                main_time_ms,
                byoyomi_ms,
                periods,
            } => {
                assert_eq!(main_time_ms, 600000); // Black to move
                assert_eq!(byoyomi_ms, 30000);
                assert_eq!(periods, 1);
            }
            _ => panic!("Expected Byoyomi time control"),
        }
    }

    #[test]
    fn test_apply_go_params_byoyomi_with_periods() {
        // Test with explicit periods
        let params = GoParams {
            byoyomi: Some(30000),
            btime: Some(600000),
            wtime: Some(600000),
            periods: Some(3),
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, 1).unwrap(); // Default 1 should be overridden

        match limits.time_control {
            TimeControl::Byoyomi {
                main_time_ms,
                byoyomi_ms,
                periods,
            } => {
                assert_eq!(main_time_ms, 600000);
                assert_eq!(byoyomi_ms, 30000);
                assert_eq!(periods, 3); // Should use periods from params, not default
            }
            _ => panic!("Expected Byoyomi time control"),
        }
    }

    #[test]
    fn test_apply_go_params_byoyomi_with_setoption_periods() {
        // Test SetOption byoyomi_periods (no periods in go command)
        let params = GoParams {
            byoyomi: Some(30000),
            btime: Some(600000),
            wtime: Some(600000),
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, 5).unwrap(); // SetOption value

        match limits.time_control {
            TimeControl::Byoyomi {
                main_time_ms,
                byoyomi_ms,
                periods,
            } => {
                assert_eq!(main_time_ms, 600000);
                assert_eq!(byoyomi_ms, 30000);
                assert_eq!(periods, 5); // Should use SetOption value
            }
            _ => panic!("Expected Byoyomi time control"),
        }
    }

    #[test]
    fn test_apply_go_params_fischer_via_byoyomi_zero() {
        // Some GUIs send byoyomi=0 with binc/winc for Fischer
        let params = GoParams {
            byoyomi: Some(0),
            btime: Some(300000),
            wtime: Some(300000),
            binc: Some(2000),
            winc: Some(2000),
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        match limits.time_control {
            TimeControl::Fischer {
                black_ms,
                white_ms,
                increment_ms,
            } => {
                assert_eq!(black_ms, 300000);
                assert_eq!(white_ms, 300000);
                assert_eq!(increment_ms, 2000); // Black to move
            }
            _ => panic!("Expected Fischer time control"),
        }
    }

    #[test]
    fn test_apply_go_params_fischer_not_mistaken_with_periods() {
        // Test that byoyomi=0 + periods doesn't trigger Fischer
        let params = GoParams {
            byoyomi: Some(0),
            periods: Some(3),
            btime: Some(300000),
            wtime: Some(300000),
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        match limits.time_control {
            TimeControl::Byoyomi { periods, .. } => {
                assert_eq!(periods, 3); // Should be Byoyomi, not Fischer
            }
            _ => panic!("Expected Byoyomi time control, not Fischer"),
        }
    }

    #[test]
    fn test_apply_go_params_fischer() {
        let params = GoParams {
            btime: Some(300000),
            wtime: Some(300000),
            binc: Some(2000),
            winc: Some(3000),
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        match limits.time_control {
            TimeControl::Fischer {
                black_ms,
                white_ms,
                increment_ms,
            } => {
                assert_eq!(black_ms, 300000);
                assert_eq!(white_ms, 300000);
                assert_eq!(increment_ms, 2000); // Black to move, so black increment
            }
            _ => panic!("Expected Fischer time control"),
        }
    }

    #[test]
    fn test_apply_go_params_depth_and_nodes() {
        let params = GoParams {
            depth: Some(20),
            nodes: Some(1000000),
            infinite: true,
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        assert_eq!(limits.depth, Some(20));
        assert_eq!(limits.node_limit(), Some(1000000));
    }

    #[test]
    fn test_apply_go_params_moves_to_go() {
        let params = GoParams {
            moves_to_go: Some(40),
            btime: Some(300000),
            wtime: Some(300000),
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        assert_eq!(limits.moves_to_go, Some(40));
    }

    #[test]
    fn test_apply_go_params_default() {
        let params = GoParams::default();
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        match limits.time_control {
            TimeControl::FixedTime { ms_per_move } => {
                assert_eq!(ms_per_move, 5000);
            }
            _ => panic!("Expected FixedTime time control with default 5000ms"),
        }
    }

    #[test]
    fn test_apply_go_params_depth_zero() {
        let params = GoParams {
            depth: Some(0),
            infinite: true,
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        // Depth 0 should be raised to 1
        assert_eq!(limits.depth, Some(1));
    }

    #[test]
    fn test_apply_go_params_depth_exceeds_max_ply() {
        let params = GoParams {
            depth: Some(200), // Exceeds MAX_PLY (127)
            infinite: true,
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        // Depth should be clamped to MAX_PLY
        assert_eq!(limits.depth, Some(MAX_PLY as u32));
    }

    #[test]
    fn test_apply_go_params_priority_ponder_over_infinite() {
        let params = GoParams {
            ponder: true,
            infinite: true,
            movetime: Some(1000),
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        match limits.time_control {
            TimeControl::Ponder => {}
            _ => panic!("Expected Ponder to take priority"),
        }
    }

    // Tests for helper functions
    #[test]
    fn test_validate_and_clamp_depth_zero() {
        assert_eq!(validate_and_clamp_depth(0), 1);
    }

    #[test]
    fn test_validate_and_clamp_depth_exceeds_max() {
        assert_eq!(validate_and_clamp_depth(200), MAX_PLY as u32);
    }

    #[test]
    fn test_validate_and_clamp_depth_normal() {
        assert_eq!(validate_and_clamp_depth(10), 10);
    }

    #[test]
    fn test_is_fischer_disguised_as_byoyomi_true() {
        let params = GoParams {
            byoyomi: Some(0),
            binc: Some(1000),
            ..Default::default()
        };
        assert!(is_fischer_disguised_as_byoyomi(&params));

        let params2 = GoParams {
            byoyomi: Some(0),
            winc: Some(1000),
            ..Default::default()
        };
        assert!(is_fischer_disguised_as_byoyomi(&params2));
    }

    #[test]
    fn test_is_fischer_disguised_as_byoyomi_false() {
        let params = GoParams {
            byoyomi: Some(30000),
            binc: Some(1000),
            ..Default::default()
        };
        assert!(!is_fischer_disguised_as_byoyomi(&params));

        let params2 = GoParams {
            byoyomi: Some(0),
            ..Default::default()
        };
        assert!(!is_fischer_disguised_as_byoyomi(&params2));

        // Test with periods - should NOT be Fischer
        let params3 = GoParams {
            byoyomi: Some(0),
            binc: Some(1000),
            periods: Some(3),
            ..Default::default()
        };
        assert!(!is_fischer_disguised_as_byoyomi(&params3));
    }

    #[test]
    fn test_get_increment_for_black() {
        let params = GoParams {
            binc: Some(2000),
            winc: Some(3000),
            ..Default::default()
        };
        assert_eq!(get_increment_for_side(&params, engine_core::shogi::Color::Black), 2000);
    }

    #[test]
    fn test_get_increment_for_white() {
        let params = GoParams {
            binc: Some(2000),
            winc: Some(3000),
            ..Default::default()
        };
        assert_eq!(get_increment_for_side(&params, engine_core::shogi::Color::White), 3000);
    }

    #[test]
    fn test_get_increment_missing_values() {
        let params = GoParams::default();
        assert_eq!(get_increment_for_side(&params, engine_core::shogi::Color::Black), 0);
        assert_eq!(get_increment_for_side(&params, engine_core::shogi::Color::White), 0);
    }

    // Tests for individual time control functions
    #[test]
    fn test_apply_ponder_mode() {
        let builder = SearchLimits::builder();
        let builder = apply_ponder_mode(builder);
        let limits = builder.build();
        match limits.time_control {
            TimeControl::Ponder => {}
            _ => panic!("Expected Ponder time control"),
        }
    }

    #[test]
    fn test_apply_infinite_mode() {
        let builder = SearchLimits::builder();
        let builder = apply_infinite_mode(builder);
        let limits = builder.build();
        match limits.time_control {
            TimeControl::Infinite => {}
            _ => panic!("Expected Infinite time control"),
        }
    }

    #[test]
    fn test_apply_fixed_time_mode() {
        let builder = SearchLimits::builder();
        let builder = apply_fixed_time_mode(builder, 1500);
        let limits = builder.build();
        match limits.time_control {
            TimeControl::FixedTime { ms_per_move } => {
                assert_eq!(ms_per_move, 1500);
            }
            _ => panic!("Expected FixedTime time control"),
        }
    }

    #[test]
    fn test_apply_default_time_control() {
        let builder = SearchLimits::builder();
        let builder = apply_default_time_control(builder);
        let limits = builder.build();
        match limits.time_control {
            TimeControl::FixedTime { ms_per_move } => {
                assert_eq!(ms_per_move, 5000);
            }
            _ => panic!("Expected FixedTime time control with 5000ms"),
        }
    }

    #[test]
    #[ignore = "Stack overflow with NNUE initialization in test environment"]
    fn test_engine_adapter_byoyomi_periods_option() {
        let mut adapter = EngineAdapter::new();

        // Default should be 1
        assert_eq!(adapter.byoyomi_periods, 1);

        // Test setting valid value
        adapter.set_option(OPT_BYOYOMI_PERIODS, Some("3")).unwrap();
        assert_eq!(adapter.byoyomi_periods, 3);

        // Test clamping to max
        adapter.set_option(OPT_BYOYOMI_PERIODS, Some("15")).unwrap();
        assert_eq!(adapter.byoyomi_periods, 10); // Should be clamped to 10

        // Test clamping to min
        adapter.set_option(OPT_BYOYOMI_PERIODS, Some("0")).unwrap();
        assert_eq!(adapter.byoyomi_periods, 1); // Should be clamped to 1

        // Test invalid value
        let result = adapter.set_option(OPT_BYOYOMI_PERIODS, Some("abc"));
        assert!(result.is_err());
        assert_eq!(adapter.byoyomi_periods, 1); // Should remain unchanged
    }

    #[test]
    fn test_apply_go_params_periods_only() {
        // Test periods specified without byoyomi
        let params = GoParams {
            periods: Some(3),
            btime: Some(300000),
            wtime: Some(300000),
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position, DEFAULT_BYOYOMI_PERIODS).unwrap();

        match limits.time_control {
            TimeControl::Byoyomi {
                main_time_ms,
                byoyomi_ms,
                periods,
            } => {
                assert_eq!(main_time_ms, 300000); // Black to move
                assert_eq!(byoyomi_ms, 0); // byoyomi defaults to 0
                assert_eq!(periods, 3); // Should use specified periods
            }
            _ => panic!("Expected Byoyomi time control"),
        }
    }
}
