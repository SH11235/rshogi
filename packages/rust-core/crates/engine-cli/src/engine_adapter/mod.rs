//! Engine adapter module for USI protocol
//!
//! This module provides a bridge between the USI protocol and the engine-core,
//! organized into submodules for better maintainability.

use engine_core::{
    engine::controller::{Engine, EngineType},
    shogi::Position,
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
    /// Whether the last search was using byoyomi time control
    last_search_is_byoyomi: bool,
    /// Quick fallback (shallow search) enable flag
    pub(crate) quick_fallback_enabled: bool,
    /// Quick fallback depth (plies)
    pub(crate) quick_fallback_depth: u8,
    /// Quick fallback time budget in ms
    pub(crate) quick_fallback_time_ms: u64,
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
            last_search_is_byoyomi: false,
            quick_fallback_enabled: false,
            quick_fallback_depth: 2,
            quick_fallback_time_ms: 30,
        };

        // Initialize options
        adapter.init_options();
        adapter
    }
}
