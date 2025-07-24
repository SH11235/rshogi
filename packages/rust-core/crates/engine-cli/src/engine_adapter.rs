//! Engine adapter for USI protocol
//!
//! This module bridges the USI protocol with the engine-core implementation,
//! handling position management, search parameter conversion, and result formatting.

use anyhow::{anyhow, Result};
use engine_core::{
    engine::controller::{Engine, EngineType},
    search::limits::SearchLimits,
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
            engine: Engine::new(EngineType::Material), // Start with material evaluator
            position: None,
            options: Vec::new(),
            hash_size: 16,
            threads: 1,
            ponder: true,
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

    /// Handle ponder hit
    pub fn ponder_hit(&mut self) {
        // TODO: Implement ponder hit logic
    }

    /// Handle game over
    pub fn game_over(&mut self, _result: GameResult) {
        // Clear position and prepare for new game
        self.position = None;
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

        // Convert GoParams to SearchLimits with info callback
        let mut builder = SearchLimits::builder();

        // Set stop flag
        builder = builder.stop_flag(stop_flag.clone());

        // Set info callback
        builder = builder.info_callback(info_callback_box);

        // Apply go parameters
        let limits = apply_go_params(builder, &params)?;
        let search_depth = limits.depth.unwrap_or(6) as u8; // Save depth before move

        log::debug!(
            "Search limits: depth={:?}, time={:?}, nodes={:?}",
            limits.depth,
            limits.time_limit(),
            limits.node_limit()
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

        // No ponder move for now
        Ok((best_move_str, None))
    }
}

/// Convert USI go parameters to basic search limits
fn apply_go_params(
    mut builder: engine_core::search::limits::SearchLimitsBuilder,
    params: &GoParams,
) -> Result<SearchLimits> {
    // Set depth limit
    if let Some(depth) = params.depth {
        builder = builder.depth(depth);
    }

    // Set node limit
    if let Some(nodes) = params.nodes {
        builder = builder.nodes(nodes);
    }

    // Set time limit
    if let Some(movetime) = params.movetime {
        builder = builder.fixed_time_ms(movetime);
    } else if params.infinite {
        // No time limit - already default
    } else {
        // Default to 5 seconds if no time specified
        builder = builder.fixed_time_ms(5000);
    }

    Ok(builder.build())
}
