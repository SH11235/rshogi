//! Position management functionality for the USI engine adapter
//! 
//! This module contains all position-related operations that were previously
//! part of the EngineAdapter implementation, extracted for better modularity.

use anyhow::Result;
use engine_core::movegen::MoveGen;
use engine_core::shogi::{Color, Move, MoveList, Position};
use engine_core::usi;

use crate::engine_adapter::{EngineAdapter, PonderState};
use crate::usi::create_position;
use crate::utils::moves_equal;

impl EngineAdapter {
    /// Check if position is set
    pub fn has_position(&self) -> bool {
        self.position.is_some()
    }

    /// Get current position
    pub fn get_position(&self) -> Option<&Position> {
        self.position.as_ref()
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

    /// Verify if a USI move string is legal in the current position
    /// Returns true if the move is legal, false otherwise
    pub fn is_legal_move(&self, usi_move: &str) -> bool {
        // Check if position is set
        let position = match &self.position {
            Some(pos) => pos,
            None => {
                log::warn!("Cannot verify move legality: no position set");
                return false;
            }
        };

        // Check for position consistency
        if let (Some(start_hash), Some(start_side)) =
            (self.search_start_position_hash, self.search_start_side_to_move)
        {
            if start_hash != position.zobrist_hash() || start_side != position.side_to_move {
                log::warn!(
                    "Position inconsistency detected during validation! Search start: hash={:#016x}, side={:?} -> Current: hash={:#016x}, side={:?}",
                    start_hash,
                    start_side,
                    position.zobrist_hash(),
                    position.side_to_move
                );
            }
        }

        // Parse USI move
        let mv = match usi::parse_usi_move(usi_move) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("Failed to parse USI move '{usi_move}': {e}");
                return false;
            }
        };

        // Generate all legal moves
        let mut generator = MoveGen::new();
        let mut legal_moves = MoveList::new();
        generator.generate_all(position, &mut legal_moves);

        // Check if the move is in the legal move list
        // Note: We need to compare moves semantically, not just by equality,
        // because USI parsing doesn't include piece type information
        for i in 0..legal_moves.len() {
            let legal_mv = legal_moves[i];
            if moves_equal(mv, legal_mv) {
                return true;
            }
        }

        // Log detailed information for debugging
        log::warn!("Move '{usi_move}' is not legal in current position");
        log::warn!("Current position SFEN: {}", usi::position_to_sfen(position));
        log::warn!("Position hash: {:#016x}, ply: {}", position.zobrist_hash(), position.ply);
        log::warn!("Side to move: {:?}", position.side_to_move);
        log::warn!("Legal moves count: {}", legal_moves.len());

        // Log first few legal moves for comparison
        if !legal_moves.is_empty() {
            log::warn!("First few legal moves:");
            for i in 0..legal_moves.len().min(10) {
                log::warn!("  {}: {}", i + 1, usi::move_to_usi(&legal_moves[i]));
            }
        }

        false
    }

    /// Clear ponder state
    pub(crate) fn clear_ponder_state(&mut self) {
        self.ponder_state.is_pondering = false;
        self.ponder_state.ponder_move = None;
        self.ponder_state.ponder_start_time = None;
        self.active_ponder_hit_flag = None;
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
}