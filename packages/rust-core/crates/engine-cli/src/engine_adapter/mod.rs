//! Engine adapter module for USI protocol
//!
//! This module provides a bridge between the USI protocol and the engine-core,
//! organized into submodules for better maintainability.

use crate::usi::GoParams;
use engine_core::{
    engine::controller::{Engine, EngineType},
    shogi::{Color, Position},
};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::usi::EngineOption;
use engine_core::time_management::constants::{
    DEFAULT_BYOYOMI_OVERHEAD_MS, DEFAULT_BYOYOMI_SAFETY_MS, DEFAULT_OVERHEAD_MS,
};

// Submodules
pub mod error;
pub mod options;
pub mod ponder;
pub mod position;
pub mod search;
pub mod time_control;
pub mod types;
pub mod utils;

// Re-export commonly used types
use engine_core::search::CommittedIteration;
use engine_core::usi::move_to_usi;
pub use error::EngineError;
pub use types::{ExtendedSearchResult, PonderState};

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
    /// Pending engine type to apply when engine is returned
    pending_engine_type: Option<EngineType>,
    /// Pending evaluation file to apply when engine is returned
    pending_eval_file: Option<String>,
    /// Current stop flag for ongoing search (shared with search worker)
    current_stop_flag: Option<Arc<AtomicBool>>,
    /// Position state at the start of search (for consistency checking)
    search_start_position_hash: Option<u64>,
    /// Side to move at the start of search
    search_start_side_to_move: Option<engine_core::shogi::Color>,
    /// Time management overhead in milliseconds
    overhead_ms: u64,
    /// Byoyomi-specific overhead in milliseconds
    byoyomi_overhead_ms: u64,
    /// Byoyomi hard limit additional safety margin in milliseconds
    byoyomi_safety_ms: u64,
    /// Minimum think time lower bound (ms)
    min_think_ms: u64,
    /// Slow mover percent (50-200)
    slow_mover_pct: u8,
    /// Max time ratio percent (100-800 for 1.00-8.00)
    max_time_ratio_pct: u32,
    /// Move horizon guard trigger (ms); 0 disables
    move_horizon_trigger_ms: u64,
    /// Move horizon minimum moves to guard; 0 disables
    move_horizon_min_moves: u32,
    /// Whether the last search was using byoyomi time control
    last_search_is_byoyomi: bool,
    /// Stochastic ponder mode (special ponderhit behavior)
    stochastic_ponder: bool,
    /// Last received GoParams (for stochastic ponder restart)
    last_go_params: Option<GoParams>,
}

impl Default for EngineAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineAdapter {
    /// Create a new engine adapter with default settings
    pub fn new() -> Self {
        // Initialize all static tables to prevent circular initialization deadlocks
        engine_core::init::init_all_tables_once();

        let mut adapter = Self {
            engine: Some(Engine::new(EngineType::Material)), // Default to Material for stability
            position: None,
            options: Vec::new(),
            hash_size: 1024,
            threads: 1,
            ponder: true,
            byoyomi_periods: None,
            byoyomi_early_finish_ratio: 80,
            pv_stability_base: 80,
            pv_stability_slope: 5,
            ponder_state: PonderState::default(),
            active_ponder_hit_flag: None,
            pending_engine_type: None,
            pending_eval_file: None,
            current_stop_flag: None,
            search_start_position_hash: None,
            search_start_side_to_move: None,
            overhead_ms: DEFAULT_OVERHEAD_MS,
            byoyomi_overhead_ms: DEFAULT_BYOYOMI_OVERHEAD_MS,
            byoyomi_safety_ms: DEFAULT_BYOYOMI_SAFETY_MS,
            min_think_ms: 200, // Phase1 default to reduce "即指し"
            slow_mover_pct: 100,
            max_time_ratio_pct: 500,
            move_horizon_trigger_ms: 0,
            move_horizon_min_moves: 0,
            last_search_is_byoyomi: false,
            stochastic_ponder: false,
            last_go_params: None,
        };

        // Initialize options
        adapter.init_options();
        adapter
    }

    /// Get overheads and tuning parameters needed for time control
    pub fn get_overheads_and_tuning(&self) -> (u32, u32, u32, u8, u64, u64) {
        (
            self.overhead_ms as u32,
            self.byoyomi_overhead_ms as u32,
            self.byoyomi_safety_ms as u32,
            self.byoyomi_early_finish_ratio,
            self.pv_stability_base,
            self.pv_stability_slope,
        )
    }

    /// Get additional time policy parameters
    pub fn get_time_policy_extras(&self) -> (u8, u32, u64, u32) {
        (
            self.slow_mover_pct,
            self.max_time_ratio_pct,
            self.move_horizon_trigger_ms,
            self.move_horizon_min_moves,
        )
    }

    /// Get MinThinkMs
    pub fn min_think_ms(&self) -> u64 {
        self.min_think_ms
    }

    /// Save last GoParams
    pub fn set_last_go_params(&mut self, params: &GoParams) {
        self.last_go_params = Some(params.clone());
    }

    /// Get last GoParams (clone)
    pub fn get_last_go_params(&self) -> Option<GoParams> {
        self.last_go_params.clone()
    }

    /// Set snapshot of search start state (for diagnostics/consistency)
    pub fn set_search_start_snapshot(&mut self, hash: u64, side: Color) {
        self.search_start_position_hash = Some(hash);
        self.search_start_side_to_move = Some(side);
    }

    /// Set flag indicating whether the last search used byoyomi time control
    pub fn set_last_search_is_byoyomi(&mut self, value: bool) {
        self.last_search_is_byoyomi = value;
    }

    /// Set the current stop flag for the ongoing search
    pub fn set_current_stop_flag(&mut self, flag: Arc<AtomicBool>) {
        self.current_stop_flag = Some(flag);
    }

    /// Begin ponder state and return a new ponder-hit flag
    pub fn begin_ponder(&mut self) -> Arc<AtomicBool> {
        self.ponder_state.is_pondering = true;
        self.ponder_state.ponder_start = Some(std::time::Instant::now());
        let flag = Arc::new(AtomicBool::new(false));
        self.active_ponder_hit_flag = Some(flag.clone());
        flag
    }

    /// Get configured number of threads (for diagnostics/logging)
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Whether Stochastic_Ponder is enabled
    pub fn is_stochastic_ponder(&self) -> bool {
        self.stochastic_ponder
    }

    /// Choose final bestmove using core decision path (book→committed→TT→legal/resign)
    /// Returns (bestmove_usi, pv_usi_vec, source_label)
    pub fn choose_final_bestmove_core(
        &self,
        committed: Option<&CommittedIteration>,
    ) -> Option<(String, Vec<String>, String)> {
        let engine = self.engine.as_ref()?;
        let pos = self.position.as_ref()?;
        let decision = engine.choose_final_bestmove(pos, committed);
        let pv_usi: Vec<String> = decision.pv.iter().map(move_to_usi).collect();
        let source = match decision.source {
            engine_core::engine::controller::FinalBestSource::Book => "book",
            engine_core::engine::controller::FinalBestSource::Committed => "committed",
            engine_core::engine::controller::FinalBestSource::TT => "tt_root",
            engine_core::engine::controller::FinalBestSource::LegalFallback => "legal_fallback",
            engine_core::engine::controller::FinalBestSource::Resign => "resign",
        };
        let bm = decision
            .best_move
            .map(|m| move_to_usi(&m))
            .unwrap_or_else(|| "resign".to_string());
        Some((bm, pv_usi, source.to_string()))
    }
}
