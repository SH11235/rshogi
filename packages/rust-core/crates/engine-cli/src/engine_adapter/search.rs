//! Search functionality for the engine adapter.
//!
//! This module handles search operations including normal search,
//! ponder search, quick search, and emergency move generation.

use anyhow::{anyhow, Result};
use engine_core::{
    engine::controller::Engine,
    movegen::MoveGenerator,
    search::{limits::SearchLimits, CommittedIteration},
    shogi::Position,
    time_management::TimeControl,
    usi::move_to_usi,
};
use log::info;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::engine_adapter::{EngineAdapter, EngineError};
use crate::usi::GoParams;

impl EngineAdapter {
    /// Take the engine out for searching
    ///
    /// This transfers ownership of the engine to the caller.
    /// The engine must be returned via `return_engine()` after use.
    pub fn take_engine(&mut self) -> Result<Engine> {
        self.engine.take().ok_or_else(|| anyhow!("Engine not available"))
    }

    /// Check if the engine is currently available
    pub fn is_engine_available(&self) -> bool {
        self.engine.is_some()
    }

    /// Return the engine after searching
    pub fn return_engine(&mut self, mut engine: Engine) {
        // Apply any pending configuration changes
        if let Some(engine_type) = self.pending_engine_type.take() {
            info!("Applying pending engine type: {engine_type:?}");
            engine.set_engine_type(engine_type);
        }

        if let Some(eval_file) = self.pending_eval_file.take() {
            info!("Applying pending eval file: {eval_file}");
            if let Err(e) = engine.load_nnue_weights(&eval_file) {
                log::error!("Failed to load pending NNUE weights: {e}");
            }
        }

        // Re-apply thread count in case it was changed
        engine.set_threads(self.threads);

        // Apply hash size if it was changed
        engine.set_hash_size(self.hash_size);

        // Return the engine
        self.engine = Some(engine);
    }

    /// Prepare search parameters
    ///
    /// Returns (position, search_limits, ponder_hit_flag)
    pub fn prepare_search(
        &mut self,
        params: &GoParams,
        stop_flag: Arc<AtomicBool>,
    ) -> Result<(Position, SearchLimits, Option<Arc<AtomicBool>>)> {
        // Check if position is set
        let position = self.get_position().ok_or_else(|| anyhow!("Position not set"))?.clone();

        // Store search start state
        self.search_start_position_hash = Some(position.zobrist_hash());
        self.search_start_side_to_move = Some(position.side_to_move);

        // Detect if this is byoyomi time control to determine overhead
        let is_byoyomi = match params {
            GoParams {
                byoyomi: Some(byo), ..
            } if *byo > 0 => {
                // Check if it's not Fischer disguised as byoyomi
                !crate::engine_adapter::time_control::is_fischer_disguised_as_byoyomi(
                    *byo,
                    params.binc,
                    params.winc,
                )
            }
            _ => false,
        };

        // Overheadの扱い: soft側を不必要に削らないため、
        // 一般オーバーヘッドはTimeParameters.overhead_msへ、
        // Byoyomi固有の追加オーバーヘッドはhard側の安全マージンとして扱う。
        // これにより soft ≒ ratio*period - overhead_ms となり、
        // 「常に period*ratio - (overhead+byoyomi_overhead)」に固定されるのを防ぐ。
        let is_byoyomi_active = is_byoyomi && !params.ponder;
        let overhead_ms: u32 = self.overhead_ms as u32;

        // Check stop flag before applying go params
        let stop_value_before = stop_flag.load(std::sync::atomic::Ordering::Acquire);
        log::info!("prepare_search: stop_flag value before apply_go_params = {stop_value_before}");

        // Compose effective byoyomi hard safety: base safety + GUI-side byoyomi overhead
        // This makes the core's hard limit reflect real wall-clock constraints (go→bestmove),
        // avoiding time losses due to pre-start latency not visible to the core timer.
        let effective_byoyomi_safety_ms: u32 = (self.byoyomi_safety_ms
            + if is_byoyomi_active {
                self.byoyomi_overhead_ms
            } else {
                0
            }) as u32;

        // Apply go parameters to get search limits with the effective safety
        let limits = crate::engine_adapter::time_control::apply_go_params(
            params,
            &position,
            overhead_ms,
            Some(stop_flag.clone()),
            effective_byoyomi_safety_ms,
            self.byoyomi_early_finish_ratio,
            self.pv_stability_base,
            self.pv_stability_slope,
        )?;

        // 監視用ログ（デバッグレベル）: byoyomi時の安全マージン適用結果
        if is_byoyomi_active {
            if let engine_core::time_management::TimeControl::Byoyomi { .. } = limits.time_control {
                log::debug!(
                    "byoyomi hard safety applied: base={}ms + overhead={}ms => effective={}ms",
                    self.byoyomi_safety_ms,
                    self.byoyomi_overhead_ms,
                    effective_byoyomi_safety_ms
                );
            }
        }

        // Check stop flag after applying go params
        let stop_value_after = stop_flag.load(std::sync::atomic::Ordering::Acquire);
        log::info!("prepare_search: stop_flag value after apply_go_params = {stop_value_after}");

        // Detect if this is actually byoyomi time control by looking at the real TimeControl
        match &limits.time_control {
            TimeControl::Byoyomi { byoyomi_ms, .. } => {
                self.last_search_is_byoyomi = true;
                let _ = byoyomi_ms; // period not tracked at adapter level anymore
            }
            TimeControl::Ponder(inner) => {
                self.last_search_is_byoyomi = matches!(**inner, TimeControl::Byoyomi { .. });
                // Do not store period on ponder (stop handler ignores bestmove on ponder stop)
            }
            _ => {
                self.last_search_is_byoyomi = false;
            }
        }

        // Setup ponder state if applicable
        let ponder_hit_flag = if params.ponder {
            self.ponder_state.is_pondering = true;
            self.ponder_state.ponder_start = Some(std::time::Instant::now());
            let flag = Arc::new(AtomicBool::new(false));
            self.active_ponder_hit_flag = Some(flag.clone());
            self.current_stop_flag = Some(stop_flag);
            Some(flag)
        } else {
            self.current_stop_flag = Some(stop_flag);
            None
        };

        Ok((position, limits, ponder_hit_flag))
    }

    // session-based validation removed

    /// Validate and get bestmove from a committed iteration (core type)
    pub fn validate_and_get_bestmove_from_committed(
        &self,
        committed: &CommittedIteration,
        position: &Position,
    ) -> Result<(String, Option<String>, crate::engine_adapter::types::PonderSource)> {
        // Get best move from PV
        let best_move =
            *committed.pv.first().ok_or_else(|| anyhow!("Empty PV in committed iteration"))?;

        let best_move_str = move_to_usi(&best_move);

        // Determine ponder move if enabled
        let (ponder_move_str, ponder_source) = if self.ponder {
            if let Some(&mv) = committed.pv.get(1) {
                (Some(move_to_usi(&mv)), crate::engine_adapter::types::PonderSource::Pv)
            } else if let Some(ref engine) = self.engine {
                if let Some(ponder_mv) = engine.get_ponder_from_tt(position, best_move) {
                    (Some(move_to_usi(&ponder_mv)), crate::engine_adapter::types::PonderSource::TT)
                } else {
                    (None, crate::engine_adapter::types::PonderSource::None)
                }
            } else {
                (None, crate::engine_adapter::types::PonderSource::None)
            }
        } else {
            (None, crate::engine_adapter::types::PonderSource::None)
        };

        Ok((best_move_str, ponder_move_str, ponder_source))
    }

    /// Perform a quick shallow search (depth 3)
    pub fn quick_search(&mut self) -> Result<String> {
        let position = self.get_position().ok_or_else(|| anyhow!("Position not set"))?.clone();

        // Take engine temporarily
        let mut engine = self.take_engine()?;

        // Use core helper for a shallow, time-bounded search
        let mv_opt =
            engine_core::util::search_helpers::quick_search_move(&mut engine, &position, 3, 100);

        // Return engine
        self.return_engine(engine);

        match mv_opt {
            Some(mv) => Ok(move_to_usi(&mv)),
            None => Err(anyhow!("No move found in quick search")),
        }
    }

    /// Check if the current position has any legal moves
    #[allow(dead_code)] // Temporarily unused due to subprocess hang issue
    pub fn has_legal_moves(&self) -> Result<bool> {
        let position = self.get_position().ok_or_else(|| anyhow!("Position not set"))?.clone();

        // Generate legal moves
        let movegen = MoveGenerator::new();
        let legal_moves = movegen
            .generate_all(&position)
            .map_err(|e| anyhow!("Failed to generate legal moves: {e}"))?;

        Ok(!legal_moves.is_empty())
    }

    /// Check if the current position has any legal moves (optimized version)
    /// Returns true as soon as the first legal move is found
    #[allow(dead_code)] // Temporarily unused due to subprocess hang issue
    pub fn has_any_legal_move(&self) -> Result<bool> {
        let position = self.get_position().ok_or_else(|| anyhow!("Position not set"))?.clone();

        // Use optimized early-exit version
        let movegen = MoveGenerator::new();
        movegen
            .has_legal_moves(&position)
            .map_err(|e| anyhow!("Failed to check legal moves: {e}"))
    }

    /// Check if the current position is in check
    #[allow(dead_code)] // Temporarily unused due to subprocess hang issue
    pub fn is_in_check(&self) -> Result<bool> {
        let position = self.get_position().ok_or_else(|| anyhow!("Position not set"))?;

        Ok(position.is_in_check())
    }

    /// Generate an emergency move using core heuristics
    pub fn generate_emergency_move(&self) -> Result<String, EngineError> {
        let position = self
            .get_position()
            .ok_or(EngineError::EngineNotAvailable("Position not set".to_string()))?;
        match engine_core::util::emergency::emergency_move_usi(position) {
            Some(s) => Ok(s),
            None => Err(EngineError::NoLegalMoves),
        }
    }

    /// Force reset engine state
    pub fn force_reset_state(&mut self) {
        // NOTE: We do NOT clear position here anymore.
        // The position should remain valid across engine resets since it represents
        // the game state from the GUI. Only the GUI should control position state
        // via position commands.

        // Clear ponder state
        self.clear_ponder_state();

        // Clear search state
        self.search_start_position_hash = None;
        self.search_start_side_to_move = None;
        self.current_stop_flag = None;

        // Reset engine if available
        if let Some(ref mut engine) = self.engine {
            // Re-apply configuration
            engine.set_threads(self.threads);
            if let Some(ref eval_file) = self.pending_eval_file {
                let _ = engine.load_nnue_weights(eval_file);
            }
        }

        info!("Engine state forcefully reset (position preserved)");
    }
}
