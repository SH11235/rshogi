//! Engine adapter for USI protocol
//!
//! This module bridges the USI protocol with the engine-core implementation,
//! handling position management, search parameter conversion, and result formatting.

use anyhow::{anyhow, Result};
use engine_core::{
    engine::controller::{Engine, EngineType},
    search::{constants::DEFAULT_SEARCH_DEPTH, limits::SearchLimits},
    shogi::Position,
};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::usi::output::{Score, SearchInfo};
use crate::usi::{create_position, EngineOption, GameResult, GoParams};

/// Engine adapter that bridges USI protocol with engine-core
pub struct EngineAdapter {
    /// The underlying engine
    engine: Engine,
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
    /// Ponder state for managing ponder searches
    ponder_state: PonderState,
}

/// State for managing ponder (think on opponent's time) functionality
#[derive(Debug, Clone, Default)]
struct PonderState {
    /// Whether currently pondering
    is_pondering: bool,
    /// The move we're pondering on (opponent's expected move)
    ponder_move: Option<String>,
    /// Original time limits before ponder mode
    original_limits: Option<GoParams>,
    /// Time spent pondering in milliseconds
    ponder_start_time: Option<std::time::Instant>,
}

impl Default for EngineAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineAdapter {
    /// Create a new engine adapter
    pub fn new() -> Self {
        let mut adapter = Self {
            engine: Engine::new(EngineType::Material), // Start with simplest for compatibility
            position: None,
            options: Vec::new(),
            hash_size: 16,
            threads: 1,
            ponder: true,
            ponder_state: PonderState::default(),
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
                    self.hash_size =
                        val.parse::<usize>().map_err(|_| anyhow!("Invalid hash size"))?;
                }
            }
            "Threads" => {
                if let Some(val) = value {
                    self.threads =
                        val.parse::<usize>().map_err(|_| anyhow!("Invalid thread count"))?;
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
                        _ => return Err(anyhow!("Invalid engine type: {}", val)),
                    };
                    self.engine.set_engine_type(engine_type);
                }
            }
            _ => {
                return Err(anyhow!("Unknown option: {}", name));
            }
        }
        Ok(())
    }

    /// Handle ponder hit (opponent played the expected move)
    pub fn ponder_hit(&mut self) -> Result<()> {
        if !self.ponder_state.is_pondering {
            return Err(anyhow!("Not currently pondering"));
        }

        // Calculate time already spent pondering
        let time_spent_ms = if let Some(start_time) = self.ponder_state.ponder_start_time {
            start_time.elapsed().as_millis() as u64
        } else {
            0
        };

        log::debug!("Ponder hit after {} ms", time_spent_ms);

        // Clear ponder state
        self.clear_ponder_state();

        // Note: The actual time adjustment is handled by TimeManager in the search
        // which was created with ponder mode and will be notified via ponder_hit()

        Ok()
    }

    /// Handle game over
    pub fn game_over(&mut self, _result: GameResult) {
        // Clear position and prepare for new game
        self.position = None;
        
        // Clear ponder state
        self.clear_ponder_state();
    }

    /// Search for best move
    pub fn search(
        &mut self,
        params: GoParams,
        stop_flag: Arc<AtomicBool>,
        info_callback: Box<dyn Fn(SearchInfo) + Send + Sync>,
    ) -> Result<(String, Option<String>)> {
        log::debug!("Starting search with params: {params:?}");

        let mut position = self.position.clone().ok_or_else(|| anyhow!("No position set"))?;

        // Set up info callback using Arc to share between closures
        let info_callback_arc = Arc::new(info_callback);
        let info_callback_clone = info_callback_arc.clone();
        type InfoCallbackType =
            Box<dyn Fn(u8, i32, u64, std::time::Duration, &[engine_core::shogi::Move]) + Send>;
        let info_callback_box: InfoCallbackType =
            Box::new(move |depth, score, nodes, elapsed, pv| {
                let pv_str: Vec<String> = pv.iter().map(engine_core::usi::move_to_usi).collect();
                let info = SearchInfo {
                    depth: Some(depth as u32),
                    time: Some(elapsed.as_millis().max(1) as u64), // Ensure at least 1ms
                    nodes: Some(nodes),
                    pv: pv_str,
                    score: Some(Score::Cp(score)),
                    ..Default::default()
                };
                info_callback_clone(info);
            });

        // Update ponder state based on search type
        if params.ponder {
            self.ponder_state.is_pondering = true;
            // Note: original_limits is reserved for future use
            // when we need to restore time limits after ponder hit
            self.ponder_state.original_limits = Some(params.clone());
            self.ponder_state.ponder_start_time = Some(std::time::Instant::now());
        } else {
            // New non-ponder search clears any existing ponder state
            self.clear_ponder_state();
        }

        // Convert GoParams to SearchLimits with info callback
        let mut builder = SearchLimits::builder();

        // Set stop flag
        builder = builder.stop_flag(stop_flag.clone());

        // Set info callback
        builder = builder.info_callback(info_callback_box);

        // Apply go parameters with position info
        let limits = apply_go_params(builder, &params, &position)?;
        let search_depth = limits.depth.unwrap_or(DEFAULT_SEARCH_DEPTH as u32) as u8; // Save depth before move

        log::debug!(
            "Search limits: time_control={:?}, depth={:?}, nodes={:?}, moves_to_go={:?}",
            limits.time_control,
            limits.depth,
            limits.node_limit(),
            limits.moves_to_go
        );

        // Run search
        log::debug!("Starting engine search...");
        let result = self.engine.search(&mut position, limits);
        log::debug!("Search completed: {result:?}");

        // Get best move
        let best_move = result.best_move.ok_or_else(|| anyhow!("No legal moves found"))?;

        // Convert move to USI format
        let best_move_str = engine_core::usi::move_to_usi(&best_move);

        log::debug!("Best move: {best_move_str}");

        // Send final info
        let info = SearchInfo {
            depth: Some(search_depth as u32), // Use the saved search depth
            time: Some(result.stats.elapsed.as_millis().max(1) as u64), // Ensure at least 1ms
            nodes: Some(result.stats.nodes),
            pv: vec![best_move_str.clone()],
            score: Some(Score::Cp(result.score)),
            ..Default::default()
        };
        info_callback_arc(info);

        // Generate ponder move if pondering is enabled and this isn't a ponder search
        let ponder_move = if self.ponder && !params.ponder {
            // Try to get the next move from PV or generate a simple prediction
            // For now, we'll return None - full implementation would analyze the position
            // after making the best move to find the most likely response
            None
        } else {
            None
        };

        // Store ponder move if we have one
        if let Some(ref pm) = ponder_move {
            self.ponder_state.ponder_move = Some(pm.clone());
        }

        Ok((best_move_str, ponder_move))
    }

    /// Clear ponder state
    fn clear_ponder_state(&mut self) {
        self.ponder_state.is_pondering = false;
        self.ponder_state.ponder_move = None;
        self.ponder_state.ponder_start_time = None;
        self.ponder_state.original_limits = None;
    }
}

/// Convert USI go parameters to search limits with proper time control
///
/// Priority order: ponder > infinite > movetime > byoyomi > fischer > default
fn apply_go_params(
    mut builder: engine_core::search::limits::SearchLimitsBuilder,
    params: &GoParams,
    position: &Position,
) -> Result<SearchLimits> {
    // Set depth limit
    if let Some(depth) = params.depth {
        builder = builder.depth(depth);
    }

    // Set node limit
    if let Some(nodes) = params.nodes {
        builder = builder.nodes(nodes);
    }

    // Set moves to go
    if let Some(moves_to_go) = params.moves_to_go {
        builder = builder.moves_to_go(moves_to_go);
    }

    // Set time control based on provided parameters
    // Priority: ponder > infinite > movetime > byoyomi > fischer > default
    if params.ponder {
        // Ponder mode
        builder = builder.ponder();
    } else if params.infinite {
        // Infinite time control
        builder = builder.infinite();
    } else if let Some(movetime) = params.movetime {
        // Fixed time per move
        builder = builder.fixed_time_ms(movetime);
    } else if let Some(byoyomi) = params.byoyomi {
        // Check if this is actually Fischer time control disguised as byoyomi
        // Some GUIs send byoyomi=0 with binc/winc for Fischer
        if byoyomi == 0 && (params.binc.is_some() || params.winc.is_some()) {
            // Treat as Fischer time control
            let black_time = params.btime.unwrap_or(0);
            let white_time = params.wtime.unwrap_or(0);
            let black_inc = params.binc.unwrap_or(0);
            let white_inc = params.winc.unwrap_or(0);

            // Use the increment for the current side
            let increment = match position.side_to_move() {
                engine_core::shogi::Color::Black => black_inc,
                engine_core::shogi::Color::White => white_inc,
            };

            // Note: fischer() expects (black_ms, white_ms, increment_ms)
            builder = builder.fischer(black_time, white_time, increment);
        } else {
            // True byoyomi time control
            let main_time = match position.side_to_move() {
                engine_core::shogi::Color::Black => params.btime.unwrap_or(0),
                engine_core::shogi::Color::White => params.wtime.unwrap_or(0),
            };
            // TODO: Add periods field to GoParams for full byoyomi support
            // Currently defaulting to 1 period
            builder = builder.byoyomi(main_time, byoyomi, 1);
        }
    } else if params.btime.is_some() || params.wtime.is_some() {
        // Fischer time control
        let black_time = params.btime.unwrap_or(0);
        let white_time = params.wtime.unwrap_or(0);
        let black_inc = params.binc.unwrap_or(0);
        let white_inc = params.winc.unwrap_or(0);

        // Use the increment for the current side
        let increment = match position.side_to_move() {
            engine_core::shogi::Color::Black => black_inc,
            engine_core::shogi::Color::White => white_inc,
        };

        // Note: fischer() expects (black_ms, white_ms, increment_ms)
        builder = builder.fischer(black_time, white_time, increment);
    } else {
        // Default to 5 seconds if no time specified
        log::warn!("No time control specified in go command, defaulting to 5 seconds");
        builder = builder.fixed_time_ms(5000);
    }

    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::shogi::Position;
    use engine_core::time_management::TimeControl;

    fn create_test_position() -> Position {
        Position::default()
    }

    #[test]
    fn test_apply_go_params_ponder() {
        let params = GoParams {
            ponder: true,
            ..Default::default()
        };
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position).unwrap();

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
        let limits = apply_go_params(builder, &params, &position).unwrap();

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
        let limits = apply_go_params(builder, &params, &position).unwrap();

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
        let limits = apply_go_params(builder, &params, &position).unwrap();

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
        let limits = apply_go_params(builder, &params, &position).unwrap();

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
        let limits = apply_go_params(builder, &params, &position).unwrap();

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
        let limits = apply_go_params(builder, &params, &position).unwrap();

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
        let limits = apply_go_params(builder, &params, &position).unwrap();

        assert_eq!(limits.moves_to_go, Some(40));
    }

    #[test]
    fn test_apply_go_params_default() {
        let params = GoParams::default();
        let position = create_test_position();
        let builder = SearchLimits::builder();
        let limits = apply_go_params(builder, &params, &position).unwrap();

        match limits.time_control {
            TimeControl::FixedTime { ms_per_move } => {
                assert_eq!(ms_per_move, 5000);
            }
            _ => panic!("Expected FixedTime time control with default 5000ms"),
        }
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
        let limits = apply_go_params(builder, &params, &position).unwrap();

        match limits.time_control {
            TimeControl::Ponder => {}
            _ => panic!("Expected Ponder to take priority"),
        }
    }
}
