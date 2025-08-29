//! Search functionality for the engine adapter.
//!
//! This module handles search operations including normal search,
//! ponder search, quick search, and emergency move generation.

use anyhow::{anyhow, Result};
use engine_core::{
    engine::controller::{Engine, EngineType},
    movegen::MoveGenerator,
    search::CommittedIteration,
    shogi::Position,
    usi::move_to_usi,
};
use log::info;

use crate::engine_adapter::{EngineAdapter, EngineError};

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

        // Ensure an engine instance exists for upcoming searches
        self.ensure_engine_available();
        info!("Engine state forcefully reset (position preserved)");
    }

    /// Ensure engine instance exists; create a fresh one if missing.
    ///
    /// This is a recovery path for rare races where the previous worker
    /// still owns the engine during a new go. A temporary engine ensures
    /// we can continue; the guard drop from the previous worker will later
    /// overwrite this instance.
    pub fn ensure_engine_available(&mut self) {
        if self.engine.is_none() {
            // Use current or default type (Material as safe baseline)
            let mut e = Engine::new(EngineType::Material);
            // Re-apply known configuration
            e.set_threads(self.threads);
            e.set_hash_size(self.hash_size);
            if let Some(ref eval_file) = self.pending_eval_file {
                let _ = e.load_nnue_weights(eval_file);
            }
            self.engine = Some(e);
            info!("EngineAdapter: created temporary engine for availability recovery");
        }
    }
}
