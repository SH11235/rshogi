//! Search functionality for the engine adapter.
//!
//! This module handles search operations including normal search,
//! ponder search, quick search, and emergency move generation.

use anyhow::{anyhow, Result};
use engine_core::{
    engine::controller::Engine,
    movegen::MoveGen,
    search::limits::SearchLimits,
    shogi::{MoveList, Position},
    usi::move_to_usi,
};
use log::{info, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::engine_adapter::{EngineAdapter, EngineError};
use crate::search_session::{Score, SearchSession};
use crate::usi::{send_info_string, GoParams};

impl EngineAdapter {
    /// Take the engine out for searching
    ///
    /// This transfers ownership of the engine to the caller.
    /// The engine must be returned via `return_engine()` after use.
    pub fn take_engine(&mut self) -> Result<Engine> {
        self.engine.take().ok_or_else(|| anyhow!("Engine not available"))
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
        self.search_start_position_hash = Some(position.hash);
        self.search_start_side_to_move = Some(position.side_to_move);

        // Calculate overhead
        let overhead_ms = if let Some(byoyomi) = params.byoyomi {
            if byoyomi > 0 {
                self.byoyomi_overhead_ms as u32
            } else {
                self.overhead_ms as u32
            }
        } else {
            self.overhead_ms as u32
        };
        self.last_overhead_ms.store(overhead_ms as u64, Ordering::Relaxed);

        // Apply go parameters to get search limits
        let limits = crate::engine_adapter::time_control::apply_go_params(
            params,
            &position,
            overhead_ms,
            Some(stop_flag.clone()),
            self.byoyomi_safety_ms as u32,
            self.byoyomi_early_finish_ratio,
            self.pv_stability_base,
            self.pv_stability_slope,
        )?;

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

    /// Validate and get bestmove from session
    pub fn validate_and_get_bestmove(
        &self,
        session: &SearchSession,
        position: &Position,
    ) -> Result<(String, Option<String>)> {
        // Check position consistency
        if session.root_hash != position.hash {
            warn!("Position hash mismatch in validate_and_get_bestmove");
            return Err(anyhow!("Position changed during search"));
        }

        // Get best move
        let best_entry = session
            .committed_best
            .as_ref()
            .ok_or_else(|| anyhow!("No best move available"))?;
        let best_move = best_entry.pv[0];
        let score = &best_entry.score;

        let best_move_str = move_to_usi(&best_move);

        // Get ponder move if available and ponder is enabled
        let ponder_move_str = if self.ponder {
            best_entry.pv.get(1).map(|&mv| move_to_usi(&mv))
        } else {
            None
        };

        // Send info about the source
        let depth = session.committed_best.as_ref().map(|b| b.depth).unwrap_or(0);
        let score_str = match score {
            Score::Cp(cp) => format!("cp {cp}"),
            Score::Mate(mate) => format!("mate {mate}"),
        };

        let _ = send_info_string(format!("bestmove_from=session depth={depth} score={score_str}"));

        Ok((best_move_str, ponder_move_str))
    }

    /// Perform a quick shallow search (depth 3)
    pub fn quick_search(&mut self) -> Result<String> {
        let position = self.get_position().ok_or_else(|| anyhow!("Position not set"))?.clone();

        // Take engine temporarily
        let mut engine = self.take_engine()?;

        // Create simple search limits
        let limits = SearchLimits::builder()
            .depth(3)
            .fixed_time_ms(100) // Max 100ms
            .build();

        // Run search
        let mut position_mut = position.clone();
        let result = engine.search(&mut position_mut, limits);

        // Return engine
        self.return_engine(engine);

        // Extract best move
        if let Some(best_move) = result.best_move {
            Ok(move_to_usi(&best_move))
        } else {
            Err(anyhow!("No move found in quick search"))
        }
    }

    /// Generate an emergency move using simple heuristics
    pub fn generate_emergency_move(&self) -> Result<String, EngineError> {
        let position = self
            .get_position()
            .ok_or(EngineError::EngineNotAvailable("Position not set".to_string()))?;

        // Generate legal moves
        let mut movegen = MoveGen::new();
        let mut legal_moves = MoveList::new();
        movegen.generate_all(position, &mut legal_moves);

        if legal_moves.is_empty() {
            return Err(EngineError::NoLegalMoves);
        }

        // Simple heuristic: prefer captures, then regular moves
        // In a real implementation, this could be more sophisticated
        let best_move = legal_moves.as_slice()[0];

        Ok(move_to_usi(&best_move))
    }

    /// Force reset engine state
    pub fn force_reset_state(&mut self) {
        // Clear position
        self.position = None;

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

        info!("Engine state forcefully reset");
    }

    /// Get the last calculated overhead in milliseconds
    pub fn get_last_overhead_ms(&self) -> u64 {
        self.last_overhead_ms.load(Ordering::Relaxed)
    }
}
