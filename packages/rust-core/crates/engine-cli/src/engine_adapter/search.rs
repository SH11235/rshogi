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
    time_management::TimeControl,
    usi::move_to_usi,
};
use log::{info, warn};
use std::sync::atomic::AtomicBool;
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

        // Use general overhead - byoyomi-specific safety margin is handled separately
        let overhead_ms = self.overhead_ms as u32;

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

        // Detect if this is actually byoyomi time control by looking at the real TimeControl
        self.last_search_is_byoyomi = match &limits.time_control {
            TimeControl::Byoyomi { .. } => true,
            TimeControl::Ponder(inner) => matches!(**inner, TimeControl::Byoyomi { .. }),
            _ => false,
        };

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

    /// Check if the last search was using byoyomi time control
    pub fn last_search_is_byoyomi(&self) -> bool {
        self.last_search_is_byoyomi
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usi::GoParams;

    fn make_test_adapter() -> EngineAdapter {
        EngineAdapter::new()
    }

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
    fn test_last_search_is_byoyomi_detection() {
        let mut adapter = make_test_adapter();
        adapter.set_position(true, None, &[]).unwrap();
        let stop_flag = Arc::new(AtomicBool::new(false));

        // Test 1: Pure byoyomi - should detect as byoyomi
        let mut params = make_go_params();
        params.byoyomi = Some(5000);
        params.btime = Some(0);
        params.wtime = Some(0);

        let (_pos, _limits, _ponder) = adapter.prepare_search(&params, stop_flag.clone()).unwrap();
        assert!(adapter.last_search_is_byoyomi(), "Pure byoyomi should be detected");

        // Test 2: Regular Fischer - should NOT detect as byoyomi
        let mut params = make_go_params();
        params.binc = Some(1000);
        params.winc = Some(2000);
        params.btime = Some(60000);
        params.wtime = Some(60000);

        let (_pos, _limits, _ponder) = adapter.prepare_search(&params, stop_flag.clone()).unwrap();
        assert!(!adapter.last_search_is_byoyomi(), "Fischer should not be detected as byoyomi");

        // Test 3: Fischer disguised as byoyomi - should NOT detect as byoyomi
        let mut params = make_go_params();
        params.byoyomi = Some(1000);
        params.binc = Some(1000);
        params.winc = Some(1000);
        params.btime = Some(60000);
        params.wtime = Some(60000);

        let (_pos, _limits, _ponder) = adapter.prepare_search(&params, stop_flag.clone()).unwrap();
        assert!(
            !adapter.last_search_is_byoyomi(),
            "Disguised Fischer should not be detected as byoyomi"
        );

        // Test 4: Ponder with inner byoyomi - should detect as byoyomi
        let mut params = make_go_params();
        params.ponder = true;
        params.byoyomi = Some(3000);
        params.btime = Some(30000);
        params.wtime = Some(30000);

        let (_pos, _limits, _ponder) = adapter.prepare_search(&params, stop_flag.clone()).unwrap();
        assert!(
            adapter.last_search_is_byoyomi(),
            "Ponder with inner byoyomi should be detected as byoyomi"
        );

        // Test 5: Ponder with inner Fischer - should NOT detect as byoyomi
        let mut params = make_go_params();
        params.ponder = true;
        params.binc = Some(1000);
        params.winc = Some(1000);
        params.btime = Some(60000);
        params.wtime = Some(60000);

        let (_pos, _limits, _ponder) = adapter.prepare_search(&params, stop_flag.clone()).unwrap();
        assert!(
            !adapter.last_search_is_byoyomi(),
            "Ponder with inner Fischer should not be detected as byoyomi"
        );
    }
}
