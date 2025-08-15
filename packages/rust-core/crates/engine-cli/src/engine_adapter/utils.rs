//! Utility functions for the engine adapter.
//!
//! This module contains helper functions for debugging, state management,
//! and static search execution.

use anyhow::Result;
use engine_core::{
    engine::controller::Engine, search::limits::SearchLimits, search::SearchResult,
    shogi::Position, usi::move_to_usi,
};
use log::{debug, error, info};
use std::sync::Arc;

use crate::engine_adapter::{EngineAdapter, EngineError, ExtendedSearchResult};
use crate::usi::{output::SearchInfo, GameResult};
use crate::utils::to_usi_score;

// Type alias for engine callback
type EngineInfoCallback =
    Arc<dyn Fn(u8, i32, u64, std::time::Duration, &[engine_core::shogi::Move]) + Send + Sync>;

impl EngineAdapter {
    /// Log current position state for debugging
    ///
    /// This method is useful for tracking position changes during search
    /// and detecting unexpected modifications.
    ///
    /// # Arguments
    /// * `context` - A descriptive string indicating where this log is called from
    pub fn log_position_state(&self, context: &str) {
        if let Some(ref pos) = self.position {
            debug!(
                "{context}: position hash={:016x}, side_to_move={:?}, ply={}",
                pos.hash, pos.side_to_move, pos.ply
            );

            // Check if position changed unexpectedly
            if let (Some(start_hash), Some(start_side)) =
                (self.search_start_position_hash, self.search_start_side_to_move)
            {
                if start_hash != pos.hash || start_side != pos.side_to_move {
                    error!(
                        "Position state changed during search! Start: hash={:016x}, side={:?} -> Current: hash={:016x}, side={:?}",
                        start_hash,
                        start_side,
                        pos.hash,
                        pos.side_to_move
                    );
                }
            }
        }
    }

    /// Handle game over notification
    ///
    /// Clears the position and ponder state to prepare for a new game.
    ///
    /// # Arguments
    /// * `_result` - The game result (win/lose/draw) - currently unused
    pub fn game_over(&mut self, _result: GameResult) {
        // Clear position and prepare for new game
        self.position = None;

        // Clear ponder state
        self.clear_ponder_state();

        info!("Game over - state cleared");
    }

    /// Clean up after search completion
    ///
    /// Resets various search-related state variables.
    ///
    /// # Arguments
    /// * `was_ponder` - Whether the completed search was a ponder search
    pub fn cleanup_after_search(&mut self, was_ponder: bool) {
        if was_ponder {
            self.active_ponder_hit_flag = None;
            if self.ponder_state.is_pondering {
                self.ponder_state.is_pondering = false;
            }
        }

        // Clear current stop flag and position state
        self.current_stop_flag = None;
        self.search_start_position_hash = None;
        self.search_start_side_to_move = None;

        debug!("Search cleanup completed (was_ponder: {was_ponder})");
    }

    /// Execute search with prepared data and return extended result
    ///
    /// This is a static method that takes ownership of the engine temporarily
    /// to perform a search operation.
    ///
    /// # Arguments
    /// * `engine` - The engine instance to use for searching
    /// * `position` - The position to search from
    /// * `limits` - Search limits and parameters
    /// * `info_callback` - Callback for search progress information
    ///
    /// # Returns
    /// * `Ok(ExtendedSearchResult)` - Search completed successfully
    /// * `Err(EngineError)` - Search failed
    pub fn execute_search_static(
        engine: &mut Engine,
        mut position: Position,
        limits: SearchLimits,
        info_callback: Box<dyn Fn(SearchInfo) + Send + Sync>,
    ) -> Result<ExtendedSearchResult, EngineError> {
        info!("execute_search_static called");
        info!("Search starting...");

        // Save original position state for verification
        let original_hash = position.hash;
        let original_side = position.side_to_move;
        let original_ply = position.ply;

        // Create info callback wrapper for engine search
        let info_callback_inner = Self::create_info_callback(info_callback);

        // Create a new SearchLimits with info_callback added
        let limits = SearchLimits {
            info_callback: Some(info_callback_inner),
            ..limits
        };

        // Debug: Check if stop_flag is present
        if let Some(ref stop_flag) = limits.stop_flag {
            info!(
                "SearchLimits has stop_flag, initial value: {}",
                stop_flag.load(std::sync::atomic::Ordering::Acquire)
            );
        } else {
            info!("WARNING: SearchLimits does not have stop_flag!");
        }

        // Execute search
        info!("Calling engine.search with limits: {limits:?}");
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.search(&mut position, limits)
        })) {
            Ok(search_result) => {
                info!(
                    "Search completed - depth:{} nodes:{} time:{}ms bestmove:{} pv_len:{}",
                    search_result.stats.depth,
                    search_result.stats.nodes,
                    search_result.stats.elapsed.as_millis(),
                    search_result
                        .best_move
                        .as_ref()
                        .map(move_to_usi)
                        .unwrap_or_else(|| "(none)".to_string()),
                    search_result.stats.pv.len()
                );
                search_result
            }
            Err(panic_info) => {
                // Try to extract panic message
                let panic_msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else {
                    "Unknown panic".to_string()
                };
                error!("PANIC in engine.search: {panic_msg}");
                return Err(EngineError::EngineNotAvailable(
                    format!("Engine panicked during search: {panic_msg}"),
                ));
            }
        };

        // Verify position wasn't modified
        if position.hash != original_hash
            || position.side_to_move != original_side
            || position.ply != original_ply
        {
            error!(
                "Position was modified during search! Original: hash={:016x}, side={:?}, ply={} -> Current: hash={:016x}, side={:?}, ply={}",
                original_hash, original_side, original_ply,
                position.hash, position.side_to_move, position.ply
            );
        }

        // Convert to extended result
        Self::convert_to_extended_result(result, &position)
    }

    /// Convert SearchResult to ExtendedSearchResult
    fn convert_to_extended_result(
        result: SearchResult,
        position: &Position,
    ) -> Result<ExtendedSearchResult, EngineError> {
        let best_move = result.best_move.ok_or_else(|| {
            error!(
                "No best move in search result. Stats: depth={}, nodes={}, elapsed={}ms, pv_len={}",
                result.stats.depth,
                result.stats.nodes,
                result.stats.elapsed.as_millis(),
                result.stats.pv.len()
            );
            if result.stats.nodes == 0 {
                EngineError::NoLegalMoves
            } else {
                EngineError::EngineNotAvailable(format!(
                    "Search completed but no best move (depth={}, nodes={}, time={}ms)",
                    result.stats.depth,
                    result.stats.nodes,
                    result.stats.elapsed.as_millis()
                ))
            }
        })?;

        // Extract PV from stats
        let pv = result.stats.pv.clone();

        // Get ponder move from PV if available
        let ponder_move = if pv.len() > 1 {
            Some(move_to_usi(&pv[1]))
        } else {
            // Try to generate a fallback ponder move
            Self::generate_ponder_fallback(position, &best_move).map(|m| move_to_usi(&m))
        };

        Ok(ExtendedSearchResult {
            best_move: move_to_usi(&best_move),
            ponder_move,
            depth: result.stats.depth as u32,
            score: result.score,
            pv,
        })
    }

    /// Generate a fallback ponder move when PV doesn't contain one
    fn generate_ponder_fallback(
        position: &Position,
        best_move: &engine_core::shogi::Move,
    ) -> Option<engine_core::shogi::Move> {
        use engine_core::movegen::MoveGen;
        use engine_core::shogi::MoveList;

        // Apply best move to get opponent's position
        let mut pos_after = position.clone();
        pos_after.do_move(*best_move);

        // Generate legal moves for opponent
        let mut movegen = MoveGen::new();
        let mut legal_moves = MoveList::new();
        movegen.generate_all(&pos_after, &mut legal_moves);

        // Return the first legal move as fallback
        if !legal_moves.is_empty() {
            Some(legal_moves.as_slice()[0])
        } else {
            None
        }
    }

    /// Create info callback wrapper for engine search
    fn create_info_callback(
        info_callback: Box<dyn Fn(SearchInfo) + Send + Sync>,
    ) -> EngineInfoCallback {
        let info_callback_arc = Arc::new(info_callback);

        Arc::new(
            move |depth: u8,
                  score: i32,
                  nodes: u64,
                  elapsed: std::time::Duration,
                  pv: &[engine_core::shogi::Move]| {
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
                (*info_callback_arc)(info);
            },
        )
    }
}
