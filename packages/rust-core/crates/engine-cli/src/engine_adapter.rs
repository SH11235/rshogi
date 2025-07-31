//! Engine adapter for USI protocol
//!
//! This module bridges the USI protocol with the engine-core implementation,
//! handling position management, search parameter conversion, and result formatting.

use anyhow::{anyhow, Context, Result};
use engine_core::{
    engine::controller::{Engine, EngineType},
    search::constants::{MATE_SCORE, MAX_PLY},
    search::limits::{SearchLimits, SearchLimitsBuilder},
    shogi::Position,
    time_management::TimeParameters,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::usi::output::{Score, SearchInfo};
use crate::usi::{
    clamp_periods, MAX_BYOYOMI_PERIODS, MIN_BYOYOMI_PERIODS, OPT_BYOYOMI_PERIODS,
    OPT_USI_BYOYOMI_PERIODS,
};
use crate::usi::{create_position, EngineOption, GameResult, GoParams};

/// Engine error types for better error handling
#[derive(Debug)]
pub enum EngineError {
    /// No legal moves available (checkmate or stalemate)
    NoLegalMoves,

    /// Engine is not available or in invalid state
    EngineNotAvailable(String),

    /// Search setup failed
    SearchSetupFailed(String),

    /// Operation timed out
    Timeout,

    /// Other errors
    Other(anyhow::Error),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::NoLegalMoves => write!(f, "No legal moves available"),
            EngineError::EngineNotAvailable(msg) => write!(f, "Engine not available: {msg}"),
            EngineError::SearchSetupFailed(msg) => write!(f, "Search setup failed: {msg}"),
            EngineError::Timeout => write!(f, "Operation timed out"),
            EngineError::Other(e) => write!(f, "Other error: {e}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<anyhow::Error> for EngineError {
    fn from(e: anyhow::Error) -> Self {
        EngineError::Other(e)
    }
}

/// Type alias for USI info callback
type UsiInfoCallback = Arc<dyn Fn(SearchInfo) + Send + Sync>;

/// Type alias for engine info callback
type EngineInfoCallback =
    Arc<dyn Fn(u8, i32, u64, std::time::Duration, &[engine_core::shogi::Move]) + Send + Sync>;

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
    /// Byoyomi periods (None = use default of 1)
    byoyomi_periods: Option<u32>,
    /// Byoyomi early finish ratio (percentage)
    byoyomi_early_finish_ratio: u8,
    /// PV stability base threshold (ms)
    pv_stability_base: u64,
    /// PV stability slope per depth (ms)
    pv_stability_slope: u64,
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

    /// Check if position is set
    pub fn has_position(&self) -> bool {
        self.position.is_some()
    }

    /// Handle new game notification
    pub fn new_game(&mut self) {
        // Clear any ponder state
        self.ponder_state = PonderState::default();
        self.active_ponder_hit_flag = None;

        // Clear position to start fresh
        self.position = None;

        // Note: Hash table clearing could be added here if engine supports it
        // For now, just log the new game
        log::debug!("New game started - cleared ponder state and position");
    }

    /// Force reset engine state to safe defaults (used for panic recovery)
    pub fn force_reset_state(&mut self) {
        log::warn!("Force resetting engine state due to error recovery");

        // Clear all ponder state
        self.clear_ponder_state();

        // Clear position - safer to require re-initialization
        log::warn!("Clearing position in force_reset_state");
        self.position = None;

        // If engine is None, we can't do much - it will be returned by EngineReturnGuard
        if self.engine.is_none() {
            log::error!("Engine is not available during reset - will be returned by guard");
        }

        // TODO: Add engine hard reset when API is available
        // This would clear:
        // - NNUE cache and network state
        // - Transposition table (hash table)
        // - History tables
        // - Killer moves
        // - Any other search-related state
        //
        // Implementation example:
        // if let Some(ref mut engine) = self.engine {
        //     engine.hard_reset()?; // Clear all internal state
        // }
        //
        // Alternative: recreate the engine entirely
        // if self.engine.is_some() {
        //     self.engine = Some(Engine::new(self.threads));
        // }

        // Try to notify GUI about the reset - we can't use USI response here
        // since we're in the adapter layer, so we'll rely on the caller to handle this
        log::info!("Engine state reset completed - please send 'isready' to reinitialize");
    }

    /// Create a new engine adapter
    pub fn new() -> Self {
        let mut adapter = Self {
            engine: Some(Engine::new(EngineType::Material)), // Default to Material for stability
            position: None,
            options: Vec::new(),
            hash_size: 16,
            threads: 1,
            ponder: true,
            byoyomi_periods: None,
            byoyomi_early_finish_ratio: 80,
            pv_stability_base: 80,
            pv_stability_slope: 5,
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
                "Material".to_string(), // Default to Material for stability
                vec![
                    "Material".to_string(),
                    "Enhanced".to_string(), // Put Enhanced before NNUE types
                    "Nnue".to_string(),
                    "EnhancedNnue".to_string(),
                ],
            ),
            EngineOption::spin(
                OPT_BYOYOMI_PERIODS,
                1,
                MIN_BYOYOMI_PERIODS as i64,
                MAX_BYOYOMI_PERIODS as i64,
            ),
            EngineOption::spin("ByoyomiEarlyFinishRatio", 80, 50, 95),
            EngineOption::spin("PVStabilityBase", 80, 10, 200),
            EngineOption::spin("PVStabilitySlope", 5, 0, 20),
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
        log::info!("Setting position - startpos: {startpos}, sfen: {sfen:?}, moves: {moves:?}");
        self.position = Some(create_position(startpos, sfen, moves)?);
        log::info!("Position set successfully");

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
            OPT_BYOYOMI_PERIODS | OPT_USI_BYOYOMI_PERIODS => {
                if let Some(val) = value {
                    if val == "default" {
                        self.byoyomi_periods = None;
                    } else {
                        let periods = val.parse::<u32>().map_err(|_| {
                            anyhow!(
                                "Invalid {}: '{}'. Must be a number between {} and {} or 'default'",
                                OPT_BYOYOMI_PERIODS,
                                val,
                                MIN_BYOYOMI_PERIODS,
                                MAX_BYOYOMI_PERIODS
                            )
                        })?;
                        self.byoyomi_periods = Some(clamp_periods(periods, false));
                    }
                } else {
                    self.byoyomi_periods = None;
                }
            }
            "ByoyomiEarlyFinishRatio" => {
                if let Some(val_str) = value {
                    let ratio = val_str.parse::<u8>().with_context(|| {
                        format!("Invalid value for ByoyomiEarlyFinishRatio: '{val_str}'. Expected integer 50-95")
                    })?;
                    if !(50..=95).contains(&ratio) {
                        return Err(anyhow!("ByoyomiEarlyFinishRatio must be between 50 and 95"));
                    }
                    self.byoyomi_early_finish_ratio = ratio;
                }
            }
            "PVStabilityBase" => {
                if let Some(val_str) = value {
                    let base = val_str.parse::<u64>().with_context(|| {
                        format!(
                            "Invalid value for PVStabilityBase: '{val_str}'. Expected integer 10-200"
                        )
                    })?;
                    if !(10..=200).contains(&base) {
                        return Err(anyhow!("PVStabilityBase must be between 10 and 200"));
                    }
                    self.pv_stability_base = base;
                }
            }
            "PVStabilitySlope" => {
                if let Some(val_str) = value {
                    let slope = val_str.parse::<u64>().with_context(|| {
                        format!(
                            "Invalid value for PVStabilitySlope: '{val_str}'. Expected integer 0-20"
                        )
                    })?;
                    if slope > 20 {
                        return Err(anyhow!("PVStabilitySlope must be between 0 and 20"));
                    }
                    self.pv_stability_slope = slope;
                }
            }
            _ => {
                return Err(anyhow!("Unknown option: '{}'. Available options: USI_Hash, Threads, USI_Ponder, EngineType, {}, {}, ByoyomiEarlyFinishRatio, PVStabilityBase, PVStabilitySlope", name, OPT_BYOYOMI_PERIODS, OPT_USI_BYOYOMI_PERIODS));
            }
        }
        Ok(())
    }

    /// Handle ponder hit (opponent played the expected move)
    pub fn ponder_hit(&mut self) -> Result<()> {
        if let Some(ref flag) = self.active_ponder_hit_flag {
            log::info!("Ponder hit: Setting flag at {:p} to true", Arc::as_ptr(flag));
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
    ) -> Result<(Position, SearchLimits, Option<Arc<AtomicBool>>), EngineError> {
        log::info!(
            "Starting {} search - depth:{:?} time:{:?}ms nodes:{:?}",
            if params.ponder { "ponder" } else { "normal" },
            params.depth,
            params.movetime.or(params.btime.or(params.wtime)),
            params.nodes
        );

        // Get position
        log::debug!("Getting current position, self.position is_some: {}", self.position.is_some());
        let position = self.position.clone().ok_or_else(|| {
            log::error!("Position is None in prepare_search!");
            EngineError::SearchSetupFailed(
                "Position not set. Use 'position startpos' or 'position sfen ...' first"
                    .to_string(),
            )
        })?;
        log::debug!("Position retrieved successfully");

        // Update ponder state and create flag if needed
        let ponder_hit_flag = if params.ponder {
            self.ponder_state.is_pondering = true;
            self.ponder_state.ponder_start_time = Some(std::time::Instant::now());
            let flag = Arc::new(AtomicBool::new(false));
            self.active_ponder_hit_flag = Some(flag.clone());
            log::info!("Ponder mode activated with shared flag at {:p}", Arc::as_ptr(&flag));
            Some(flag)
        } else {
            self.clear_ponder_state();
            None
        };

        // Create SearchLimits
        log::debug!("Creating SearchLimits");
        let mut builder = SearchLimits::builder();

        // Set stop flag
        builder = builder.stop_flag(stop_flag);

        // Set ponder hit flag if available
        if let Some(ref flag) = ponder_hit_flag {
            log::debug!("Setting ponder_hit_flag in SearchLimitsBuilder");
            builder = builder.ponder_hit_flag(flag.clone());
        } else {
            log::debug!("No ponder_hit_flag to set in SearchLimitsBuilder");
        }

        // Create TimeParameters from engine settings
        log::debug!("Creating TimeParameters");
        // Use larger overhead for byoyomi mode to ensure we finish before GUI timeout
        let overhead_ms = if params.byoyomi.is_some() { 500 } else { 50 };
        let time_params = TimeParameters {
            byoyomi_soft_ratio: self.byoyomi_early_finish_ratio as f64 / 100.0,
            pv_base_threshold_ms: self.pv_stability_base,
            pv_depth_slope_ms: self.pv_stability_slope,
            overhead_ms,
            byoyomi_hard_limit_reduction_ms: 300, // Additional safety margin for byoyomi
            ..Default::default()
        };

        // Set time parameters
        builder = builder.time_parameters(time_params);

        // Apply go parameters
        // Use periods from go command if specified, otherwise use SetOption value (or default to 1)
        let periods = params.periods.unwrap_or(self.byoyomi_periods.unwrap_or(1));
        log::debug!("Applying go parameters with periods: {periods}");
        let limits = apply_go_params(builder, params, &position, periods).map_err(|e| {
            EngineError::SearchSetupFailed(format!("Failed to apply go parameters: {e}"))
        })?;
        log::debug!("SearchLimits created successfully");

        Ok((position, limits, ponder_hit_flag))
    }

    /// Create info callback wrapper for engine search
    fn create_info_callback(
        info_callback: Box<dyn Fn(SearchInfo) + Send + Sync>,
    ) -> (UsiInfoCallback, EngineInfoCallback) {
        let info_callback_arc = Arc::new(info_callback);
        let info_callback_clone = info_callback_arc.clone();

        let info_callback_inner = Arc::new(
            move |depth: u8,
                  score: i32,
                  nodes: u64,
                  elapsed: std::time::Duration,
                  pv: &[engine_core::shogi::Move]| {
                // TODO: Consider reusing Vec<String> or using SmallVec for performance
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
            },
        );

        (info_callback_arc, info_callback_inner)
    }

    /// Process search result and send final info
    fn process_search_result(
        result: &engine_core::search::types::SearchResult,
        info_callback_arc: &UsiInfoCallback,
        position: &Position,
    ) -> Result<(String, Option<engine_core::shogi::Move>)> {
        // Get best move
        let best_move = result.best_move.ok_or_else(|| {
            anyhow!("No legal moves available in this position (checkmate or stalemate)")
        })?;

        // Convert move to USI format
        let best_move_str = engine_core::usi::move_to_usi(&best_move);

        // Extract ponder move from PV if available
        let mut ponder_move = if result.stats.pv.len() >= 2 {
            Some(result.stats.pv[1])
        } else {
            // Fallback: Generate plausible ponder move
            Self::generate_ponder_fallback(position, &best_move)
        };

        // Validate ponder move
        if let Some(ref pm) = ponder_move {
            if !Self::is_valid_ponder_move(position, &best_move, pm) {
                log::debug!("Invalid ponder move detected, regenerating with fallback");
                ponder_move = Self::generate_ponder_fallback(position, &best_move);
            }
        }

        log::debug!(
            "Best move: {} (score: {:?}) ponder: {:?}",
            best_move_str,
            result.score,
            ponder_move
        );

        // Send final info
        let score = to_usi_score(result.score);
        let depth = if result.stats.depth == 0 {
            // This can happen when search is interrupted very early or in some edge cases
            // TODO: Investigate root cause - might be related to time control or early termination
            log::warn!("SearchStats.depth is 0, this should not happen. Using 1 as fallback.");
            1
        } else {
            result.stats.depth
        };

        // Convert full PV to USI format for final info
        let pv_usi: Vec<String> =
            result.stats.pv.iter().map(engine_core::usi::move_to_usi).collect();

        let info = SearchInfo {
            depth: Some(depth as u32),
            time: Some(result.stats.elapsed.as_millis().max(1) as u64),
            nodes: Some(result.stats.nodes),
            pv: pv_usi,
            score: Some(score),
            ..Default::default()
        };
        (*info_callback_arc)(info);

        Ok((best_move_str, ponder_move))
    }

    /// Generate fallback ponder move when PV is too short
    fn generate_ponder_fallback(
        position: &Position,
        best_move: &engine_core::shogi::Move,
    ) -> Option<engine_core::shogi::Move> {
        use engine_core::movegen::MoveGen;
        use engine_core::shogi::MoveList;

        // Create a copy of the position to make the move
        let mut temp_position = position.clone();

        // Apply the best move
        temp_position.do_move(*best_move);

        // Generate legal moves in the new position (opponent's moves)
        let mut generator = MoveGen::new();
        let mut moves = MoveList::new();
        generator.generate_all(&temp_position, &mut moves);

        if moves.is_empty() {
            // Position after best move is checkmate or stalemate
            return None;
        }

        // Simple heuristic: prefer captures, checks, and central moves
        let ponder_move = Self::select_plausible_move(&temp_position, &moves);

        log::debug!(
            "Generated fallback ponder move: {:?} from {} candidates",
            ponder_move,
            moves.len()
        );

        ponder_move
    }

    /// Select a plausible move from candidates using simple heuristics
    fn select_plausible_move(
        position: &Position,
        moves: &engine_core::shogi::MoveList,
    ) -> Option<engine_core::shogi::Move> {
        // First pass: Look for captures
        for i in 0..moves.len() {
            let mv = moves[i];
            // Check if target square has opponent piece
            if position.board.piece_on(mv.to()).is_some() {
                return Some(mv);
            }
        }

        // Second pass: Look for checks
        for i in 0..moves.len() {
            let mv = moves[i];
            let mut temp_pos = position.clone();
            temp_pos.do_move(mv);
            if temp_pos.in_check() {
                return Some(mv);
            }
        }

        // Third pass: Return the first legal move
        if !moves.is_empty() {
            Some(moves[0])
        } else {
            None
        }
    }

    /// Validate that ponder move is legal in the position after best move
    fn is_valid_ponder_move(
        position: &Position,
        best_move: &engine_core::shogi::Move,
        ponder_move: &engine_core::shogi::Move,
    ) -> bool {
        use engine_core::movegen::MoveGen;
        use engine_core::shogi::MoveList;

        // Create a copy of the position and apply the best move
        let mut temp_position = position.clone();
        temp_position.do_move(*best_move);

        // Generate all legal moves in the new position
        let mut generator = MoveGen::new();
        let mut moves = MoveList::new();
        generator.generate_all(&temp_position, &mut moves);

        // Check if ponder move is in the legal moves list
        for i in 0..moves.len() {
            if moves[i] == *ponder_move {
                return true;
            }
        }

        false
    }

    /// Execute search with prepared data and return USI result
    /// This takes a mutable reference to avoid ownership transfer
    pub fn execute_search_static(
        engine: &mut Engine,
        mut position: Position,
        limits: SearchLimits,
        info_callback: Box<dyn Fn(SearchInfo) + Send + Sync>,
    ) -> Result<(String, Option<String>), EngineError> {
        log::info!("execute_search_static called");
        log::info!("Search starting...");

        // Set up info callback
        let (info_callback_arc, info_callback_inner) = Self::create_info_callback(info_callback);

        // Create a new SearchLimits with info_callback added
        // We can't add it in prepare_search because the callback is created here
        // This is not shadowing - we're creating a new instance with the callback
        let limits = SearchLimits {
            info_callback: Some(info_callback_inner),
            ..limits
        };

        // Execute search
        log::info!("Calling engine.search");
        let result = engine.search(&mut position, limits);
        log::info!(
            "Search completed - depth:{} nodes:{} time:{}ms bestmove:{}",
            result.stats.depth,
            result.stats.nodes,
            result.stats.elapsed.as_millis(),
            result.best_move.is_some()
        );

        // Process result
        let (best_move_str, ponder_move) =
            Self::process_search_result(&result, &info_callback_arc, &position)
                .map_err(EngineError::Other)?;

        // Convert ponder move to USI format if available
        let ponder_move_str = ponder_move.map(|m| engine_core::usi::move_to_usi(&m));

        Ok((best_move_str, ponder_move_str))
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

    /// Validate engine state before attempting emergency move generation
    fn validate_engine_state(&self) -> Result<(), EngineError> {
        // Check if position is set
        if self.position.is_none() {
            return Err(EngineError::EngineNotAvailable("Position not set".to_string()));
        }

        // Check if we're in a valid state (not in the middle of a search)
        // This is a simple check - could be expanded based on actual state requirements

        Ok(())
    }

    /// Generate an emergency move when normal search fails or times out
    /// Returns the first legal move found, or an error if no legal moves exist
    pub fn generate_emergency_move(&self) -> Result<String, EngineError> {
        use engine_core::movegen::MoveGen;
        use engine_core::shogi::MoveList;

        // IMPORTANT: This is a synchronous, lightweight operation that doesn't use search.
        // It only generates legal moves and selects one using simple heuristics.
        // Typical execution time: < 1ms
        // No timeout mechanism needed as this is guaranteed to be fast.

        // Validate engine state first
        self.validate_engine_state()?;

        // Get current position
        let position = self
            .position
            .as_ref()
            .ok_or(EngineError::EngineNotAvailable("No position set".to_string()))?;

        // Check if we're in check - if so, prioritize king safety
        let in_check = position.in_check();

        // Generate legal moves for the current position
        let mut generator = MoveGen::new();
        let mut moves = MoveList::new();
        generator.generate_all(position, &mut moves);

        if moves.is_empty() {
            // No legal moves - this is checkmate or stalemate
            return Err(EngineError::NoLegalMoves);
        }

        log::info!("Emergency move generation: {} legal moves, in_check={}", moves.len(), in_check);

        // If in check and only one legal move, use it immediately
        if in_check && moves.len() == 1 {
            let move_str = engine_core::usi::move_to_usi(&moves[0]);
            log::info!("Only one legal move to escape check: {move_str}");
            return Ok(move_str);
        }

        // Select a plausible move using enhanced logic
        let selected_move = if in_check {
            // In check: prioritize king safety
            Self::select_check_escape_move(position, &moves)
        } else {
            // Not in check: use normal heuristics
            Self::select_plausible_move(position, &moves)
        }
        .ok_or_else(|| {
            EngineError::Other(anyhow!("Failed to select move despite having legal moves"))
        })?;

        // Convert to USI format
        let move_str = engine_core::usi::move_to_usi(&selected_move);

        log::info!(
            "Generated emergency move: {} (from {} legal moves, in_check={})",
            move_str,
            moves.len(),
            in_check
        );

        Ok(move_str)
    }

    /// Perform a quick shallow search (depth 1-3) for emergency situations
    /// This is used as a fallback when the main search fails or times out
    pub fn quick_search(&mut self) -> Result<String, EngineError> {
        use engine_core::search::limits::SearchLimits;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        // Validate engine state first
        self.validate_engine_state()?;

        // Get current position
        let mut position = self
            .position
            .as_ref()
            .ok_or(EngineError::EngineNotAvailable("No position set".to_string()))?
            .clone();

        // Check if engine is available
        let engine = self.engine.as_mut().ok_or(EngineError::EngineNotAvailable(
            "Engine not available for quick search".to_string(),
        ))?;

        // Create a stop flag for emergency timeout
        let stop_flag = Arc::new(AtomicBool::new(false));

        // Create minimal search limits - prioritize depth for reliability
        // Note: We set depth=3 as primary constraint for quick shallow search.
        // The 100ms time limit acts as a safety net to prevent hanging.
        //
        // IMPORTANT: SearchLimits design allows both depth and time constraints.
        // They work independently - the search stops when EITHER limit is reached:
        // - Stop at depth 3 (primary goal for consistent quality)
        // - OR stop at 100ms if depth 3 takes too long (safety against hanging)
        //
        // The builder maintains both values separately:
        // - depth is stored in its own field
        // - fixed_time_ms sets the time_control field to FixedTime
        // The order of builder calls doesn't matter - both constraints remain active.
        //
        // In practice, depth 3 should complete well within 100ms, making this
        // primarily a depth-limited search with timeout protection.
        let limits = SearchLimits::builder()
            .depth(3)
            .fixed_time_ms(100)
            .stop_flag(stop_flag.clone())
            .build();

        log::info!("Starting quick search (depth 3 with 100ms safety timeout)");

        // Execute search
        // IMPORTANT: Currently this is a synchronous call that relies on the engine
        // implementation to respect the time limit. The stop_flag is provided but
        // there's no separate thread monitoring the timeout.
        //
        // Current behavior:
        // - engine.search() should internally check the stop_flag and time limit
        // - For depth 3, this typically completes within 10-50ms on modern hardware
        // - The 100ms limit provides a safety margin
        //
        // Future improvement options:
        // 1. Spawn a monitoring thread that sets stop_flag after timeout
        // 2. Use tokio::time::timeout with async search
        // 3. Rely on engine's internal time management (current approach)
        //
        // For now, we trust the engine implementation to handle timeouts correctly.
        let result = engine.search(&mut position, limits);

        // Check if we found a move
        if result.best_move.is_none() {
            return Err(EngineError::NoLegalMoves);
        }

        // Convert to USI format
        let best_move = result.best_move.unwrap();
        let move_str = engine_core::usi::move_to_usi(&best_move);

        log::info!(
            "Quick search completed: {} (depth:{} nodes:{} score:{})",
            move_str,
            result.stats.depth,
            result.stats.nodes,
            result.score
        );

        Ok(move_str)
    }

    /// Select a move to escape check, prioritizing safety
    fn select_check_escape_move(
        position: &Position,
        moves: &engine_core::shogi::MoveList,
    ) -> Option<engine_core::shogi::Move> {
        // First pass: Look for captures that also escape check
        for i in 0..moves.len() {
            let mv = moves[i];
            if position.board.piece_on(mv.to()).is_some() {
                // Verify it escapes check
                let mut temp_pos = position.clone();
                temp_pos.do_move(mv);
                if !temp_pos.in_check() {
                    return Some(mv);
                }
            }
        }

        // Second pass: Any move that escapes check
        for i in 0..moves.len() {
            let mv = moves[i];
            let mut temp_pos = position.clone();
            temp_pos.do_move(mv);
            if !temp_pos.in_check() {
                return Some(mv);
            }
        }

        // This shouldn't happen if moves are legal, but return first move as fallback
        if !moves.is_empty() {
            Some(moves[0])
        } else {
            None
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
    let byoyomi = params.byoyomi.unwrap_or(0);
    let main_time = match position.side_to_move {
        engine_core::shogi::Color::Black => params.btime.unwrap_or(byoyomi),
        engine_core::shogi::Color::White => params.wtime.unwrap_or(byoyomi),
    };
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
        log::debug!("Periods specified without byoyomi, using Byoyomi mode with byoyomi=0");
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
    let effective_periods = params.periods.unwrap_or(byoyomi_periods);

    let builder = apply_time_control(builder, params, position, effective_periods);
    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::movegen::MoveGen;
    use engine_core::shogi::{Move, MoveList, Position};
    use engine_core::time_management::TimeControl;
    use engine_core::Square;

    const DEFAULT_BYOYOMI_PERIODS: u32 = 1;

    // Helper function to parse and validate USI move string
    fn parse_and_validate_move(position: &Position, usi_move: &str) -> Result<Move> {
        // Generate legal moves
        let mut move_gen = MoveGen::new();
        let mut legal_moves = MoveList::new();
        move_gen.generate_all(position, &mut legal_moves);

        if legal_moves.is_empty() {
            return Err(anyhow::anyhow!("No legal moves in position"));
        }

        // Find the matching legal move by USI string comparison
        for i in 0..legal_moves.len() {
            let legal_move = legal_moves[i];
            let legal_usi = engine_core::usi::move_to_usi(&legal_move);
            if legal_usi == usi_move {
                return Ok(legal_move);
            }
        }

        Err(anyhow::anyhow!("Move {} is not legal in current position", usi_move))
    }

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
            TimeControl::Ponder(_) => {}
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
            TimeControl::Ponder(_) => {}
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
    // #[test]
    // fn test_apply_ponder_mode() {
    //     let builder = SearchLimits::builder();
    //     let builder = apply_ponder_mode(builder);
    //     let limits = builder.build();
    //     match limits.time_control {
    //         TimeControl::Ponder(_) => {}
    //         _ => panic!("Expected Ponder time control"),
    //     }
    // }

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

        // Default should be None
        assert_eq!(adapter.byoyomi_periods, None);

        // Test setting valid value
        adapter.set_option(OPT_BYOYOMI_PERIODS, Some("3")).unwrap();
        assert_eq!(adapter.byoyomi_periods, Some(3));

        // Test clamping to max
        adapter.set_option(OPT_BYOYOMI_PERIODS, Some("15")).unwrap();
        assert_eq!(adapter.byoyomi_periods, Some(10)); // Should be clamped to 10

        // Test clamping to min
        adapter.set_option(OPT_BYOYOMI_PERIODS, Some("0")).unwrap();
        assert_eq!(adapter.byoyomi_periods, Some(1)); // Should be clamped to 1

        // Test resetting to default
        adapter.set_option(OPT_BYOYOMI_PERIODS, Some("default")).unwrap();
        assert_eq!(adapter.byoyomi_periods, None);

        // Test invalid value
        let result = adapter.set_option(OPT_BYOYOMI_PERIODS, Some("abc"));
        assert!(result.is_err());
        assert_eq!(adapter.byoyomi_periods, None); // Should remain unchanged
    }

    #[test]
    #[ignore = "Stack overflow with NNUE initialization in test environment"]
    fn test_engine_adapter_byoyomi_periods_alias() {
        let mut adapter = EngineAdapter::new();

        // Test USI_ByoyomiPeriods alias
        adapter.set_option(OPT_USI_BYOYOMI_PERIODS, Some("5")).unwrap();
        assert_eq!(adapter.byoyomi_periods, Some(5));

        // Test both options refer to same value
        adapter.set_option(OPT_BYOYOMI_PERIODS, Some("7")).unwrap();
        assert_eq!(adapter.byoyomi_periods, Some(7));
    }

    #[test]
    fn test_time_parameters_creation_in_prepare_search() {
        let params = GoParams {
            byoyomi: Some(30000),
            btime: Some(600000),
            wtime: Some(600000),
            ..Default::default()
        };
        let position = create_test_position();

        // Test with default values
        let builder = SearchLimits::builder();
        let time_params = TimeParameters {
            byoyomi_soft_ratio: 0.8,  // 80% default
            pv_base_threshold_ms: 80, // default
            pv_depth_slope_ms: 5,     // default
            ..Default::default()
        };
        let builder = builder.time_parameters(time_params);
        let limits = apply_go_params(builder, &params, &position, 1).unwrap();

        // Verify TimeParameters were set
        assert!(limits.time_parameters.is_some());
        let tp = limits.time_parameters.unwrap();
        assert_eq!(tp.byoyomi_soft_ratio, 0.8);
        assert_eq!(tp.pv_base_threshold_ms, 80);
        assert_eq!(tp.pv_depth_slope_ms, 5);

        // Test with custom values
        let builder2 = SearchLimits::builder();
        let time_params2 = TimeParameters {
            byoyomi_soft_ratio: 0.9, // 90%
            pv_base_threshold_ms: 100,
            pv_depth_slope_ms: 10,
            ..Default::default()
        };
        let builder2 = builder2.time_parameters(time_params2);
        let limits2 = apply_go_params(builder2, &params, &position, 1).unwrap();

        // Verify custom TimeParameters were set
        assert!(limits2.time_parameters.is_some());
        let tp2 = limits2.time_parameters.unwrap();
        assert_eq!(tp2.byoyomi_soft_ratio, 0.9);
        assert_eq!(tp2.pv_base_threshold_ms, 100);
        assert_eq!(tp2.pv_depth_slope_ms, 10);
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

    #[test]
    fn test_process_search_result_with_pv() {
        use engine_core::search::types::{SearchResult, SearchStats};
        use std::time::Duration;

        // Create test position and moves using known valid moves
        let position = Position::startpos();
        // Using actual valid moves from the initial position
        // Black pawns are at rank g (6), move toward rank a (0)
        let move1 = parse_and_validate_move(&position, "7g7f").unwrap();

        let mut pos2 = position.clone();
        pos2.do_move(move1);
        // After black 7g7f, white can respond
        let move2 = parse_and_validate_move(&pos2, "3c3d").unwrap();

        let mut pos3 = pos2.clone();
        pos3.do_move(move2);
        // After white 3c3d, black can continue
        let move3 = parse_and_validate_move(&pos3, "8g8f").unwrap();

        let result = SearchResult {
            best_move: Some(move1),
            score: 100,
            stats: SearchStats {
                nodes: 1000,
                elapsed: Duration::from_millis(100),
                pv: vec![move1, move2, move3],
                depth: 3,
                ..Default::default()
            },
        };

        // Create a dummy callback
        let info_callback: Arc<dyn Fn(SearchInfo) + Send + Sync> = Arc::new(|_: SearchInfo| {});

        // Process result
        let (best_move_str, ponder_move) =
            EngineAdapter::process_search_result(&result, &info_callback, &position).unwrap();

        // Verify
        assert_eq!(best_move_str, engine_core::usi::move_to_usi(&move1));
        assert!(ponder_move.is_some());
        assert_eq!(ponder_move.unwrap(), move2);
    }

    #[test]
    fn test_process_search_result_without_ponder() {
        use engine_core::search::types::{SearchResult, SearchStats};
        use std::time::Duration;

        // Create test position and move
        let position = Position::startpos();
        // Black pawn at rank g (6) moves to rank f (5)
        let move1 = parse_and_validate_move(&position, "7g7f").unwrap();

        let result = SearchResult {
            best_move: Some(move1),
            score: 50,
            stats: SearchStats {
                nodes: 10,
                elapsed: Duration::from_millis(10),
                pv: vec![move1], // Only one move in PV
                depth: 1,
                ..Default::default()
            },
        };

        // Create a dummy callback
        let info_callback: Arc<dyn Fn(SearchInfo) + Send + Sync> = Arc::new(|_: SearchInfo| {});

        // Process result
        let (best_move_str, ponder_move) =
            EngineAdapter::process_search_result(&result, &info_callback, &position).unwrap();

        // Verify - with fallback, we should get a ponder move
        assert_eq!(best_move_str, engine_core::usi::move_to_usi(&move1));
        assert!(ponder_move.is_some(), "Fallback should generate ponder move");
    }

    #[test]
    fn test_process_search_result_empty_pv() {
        use engine_core::search::types::{SearchResult, SearchStats};
        use std::time::Duration;

        // Create test position and move
        let position = Position::startpos();
        // Use a Black move since Black moves first
        let move1 = parse_and_validate_move(&position, "7g7f").unwrap();

        let result = SearchResult {
            best_move: Some(move1),
            score: 0,
            stats: SearchStats {
                nodes: 1,
                elapsed: Duration::from_millis(1),
                pv: vec![], // Empty PV
                depth: 0,
                ..Default::default()
            },
        };

        // Create a dummy callback
        let info_callback: Arc<dyn Fn(SearchInfo) + Send + Sync> = Arc::new(|_: SearchInfo| {});

        // Process result
        let (best_move_str, ponder_move) =
            EngineAdapter::process_search_result(&result, &info_callback, &position).unwrap();

        // Verify - with fallback, we should get a ponder move
        assert_eq!(best_move_str, engine_core::usi::move_to_usi(&move1));
        assert!(ponder_move.is_some(), "Fallback should generate ponder move");
    }

    #[test]
    fn test_ponder_fallback_generation() {
        // Test fallback ponder move generation
        let position = Position::startpos();

        // Use a move that exists in the initial position
        // Black pawn at rank g (6) moves to rank f (5)
        let best_move = parse_and_validate_move(&position, "7g7f").unwrap();

        let ponder = EngineAdapter::generate_ponder_fallback(&position, &best_move);

        assert!(ponder.is_some(), "Should generate fallback ponder move");

        // Verify the ponder move is valid
        let ponder_move = ponder.unwrap();
        assert!(EngineAdapter::is_valid_ponder_move(&position, &best_move, &ponder_move));
    }

    #[test]
    fn test_ponder_move_validation() {
        let position = Position::startpos();
        // Black pawn at rank g (6) moves to rank f (5)
        let best_move = parse_and_validate_move(&position, "7g7f").unwrap();

        // Create position after best move
        let mut pos_after = position.clone();
        pos_after.do_move(best_move);

        // Valid ponder move (opponent's response)
        // After 7g7f (black pawn advance), white can respond
        let valid_ponder = parse_and_validate_move(&pos_after, "3c3d").unwrap();
        let is_valid = EngineAdapter::is_valid_ponder_move(&position, &best_move, &valid_ponder);
        assert!(is_valid, "3c3d should be valid after 7g7f");

        // Another valid opponent move
        let another_valid = parse_and_validate_move(&pos_after, "8c8d").unwrap();
        assert!(
            EngineAdapter::is_valid_ponder_move(&position, &best_move, &another_valid),
            "8c8d should be valid"
        );

        // Invalid move - trying to parse our color's move should fail in opponent's turn
        // So we test with an illegal move instead
        let illegal_from = "9a".parse::<Square>().unwrap();
        let illegal_to = "1i".parse::<Square>().unwrap();
        let invalid_ponder = Move::normal(illegal_from, illegal_to, false);
        assert!(
            !EngineAdapter::is_valid_ponder_move(&position, &best_move, &invalid_ponder),
            "Illegal move should be invalid"
        );
    }

    #[test]
    #[ignore = "Requires actual engine instance - run with --ignored flag"]
    fn test_quick_search_timeout_guarantee() {
        use std::time::Instant;

        // NOTE: This test requires an actual engine instance to run
        // It's marked as ignored to prevent stack overflow in regular test runs
        // Run with: cargo test --bin engine-cli test_quick_search_timeout_guarantee -- --ignored

        // Create adapter with a test position
        let mut adapter = EngineAdapter::new();
        adapter.new_game();

        // Set a test position using startpos
        adapter.set_position(true, None, &[]).expect("Failed to set position");

        // Measure execution time
        let start = Instant::now();
        let result = adapter.quick_search();
        let elapsed = start.elapsed();

        // Verify it completes within reasonable time (150ms with margin)
        assert!(
            elapsed.as_millis() < 150,
            "quick_search took too long: {}ms (expected < 150ms)",
            elapsed.as_millis()
        );

        // Verify we got a valid result
        assert!(result.is_ok(), "quick_search should return Ok");
        let move_str = result.unwrap();
        assert!(!move_str.is_empty(), "quick_search should return a move");

        // Run multiple times to ensure consistency
        for i in 0..5 {
            let start = Instant::now();
            let _result = adapter.quick_search();
            let elapsed = start.elapsed();

            assert!(
                elapsed.as_millis() < 150,
                "quick_search iteration {} took too long: {}ms",
                i + 1,
                elapsed.as_millis()
            );
        }
    }
}
