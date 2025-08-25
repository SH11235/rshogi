//! Position management functionality for the engine adapter.
//!
//! This module handles position state management, move validation,
//! and position updates from USI commands.

use anyhow::{Context, Result};
use engine_core::movegen::MoveGen;
use engine_core::shogi::{MoveList, Position};
use engine_core::usi::parse_usi_move;
use log::{debug, info, warn};

use crate::engine_adapter::{EngineAdapter, PonderState};
use crate::usi::create_position;

impl EngineAdapter {
    /// Check if a position is currently set
    pub fn has_position(&self) -> bool {
        self.position.is_some()
    }

    /// Get a reference to the current position
    pub fn get_position(&self) -> Option<&Position> {
        self.position.as_ref()
    }

    /// Set position from USI position command
    ///
    /// # Arguments
    /// * `startpos` - Whether to use the starting position
    /// * `sfen` - SFEN string if not using startpos
    /// * `moves` - List of moves to apply after setting the position
    ///
    /// # Returns
    /// * `Ok(())` if position was set successfully
    /// * `Err` if position parsing or move application failed
    pub fn set_position(
        &mut self,
        startpos: bool,
        sfen: Option<&str>,
        moves: &[String],
    ) -> Result<()> {
        // Create the position with moves applied
        let position =
            create_position(startpos, sfen, moves).context("Failed to create position")?;

        // Clear ponder state when setting a new position
        self.clear_ponder_state();

        // Store the position
        self.position = Some(position);

        info!(
            "Position set: {} with {} moves",
            if startpos { "startpos" } else { "sfen" },
            moves.len()
        );

        Ok(())
    }

    /// Check if a USI move string is legal in the current position
    ///
    /// # Arguments
    /// * `usi_move` - Move string in USI format (e.g., "7g7f", "2b3c+", "P*5e")
    ///
    /// # Returns
    /// * `true` if the move is legal in the current position
    /// * `false` if no position is set or the move is illegal
    pub fn is_legal_move(&self, usi_move: &str) -> bool {
        let Some(position) = &self.position else {
            warn!("Cannot check move legality: no position set");
            return false;
        };

        // Try to parse the USI move
        let Ok(parsed_move) = parse_usi_move(usi_move) else {
            debug!("Failed to parse USI move: {usi_move}");
            return false;
        };

        // Generate legal moves
        let mut movegen = MoveGen::new();
        let mut legal_moves = MoveList::new();
        movegen.generate_all(position, &mut legal_moves);

        // Check if the parsed move matches any legal move
        // Note: We need to compare moves semantically (ignoring piece type encoding)
        // because USI notation doesn't include piece type information
        let is_legal = legal_moves.as_slice().iter().any(|&legal_move| {
            // Basic move matching
            parsed_move.from() == legal_move.from()
                && parsed_move.to() == legal_move.to()
                && parsed_move.is_drop() == legal_move.is_drop()
                && (!parsed_move.is_drop()
                    || parsed_move.drop_piece_type() == legal_move.drop_piece_type())
                && (
                    // Either promotion flags match exactly
                    parsed_move.is_promote() == legal_move.is_promote()
                    // OR the parsed move tries to promote but the legal move doesn't allow it
                    // (This handles cases like "2b8h+" where promotion is impossible)
                    || (parsed_move.is_promote() && !legal_move.is_promote())
                )
        });

        if !is_legal {
            debug!("Move {usi_move} is not legal in current position (parsed as {parsed_move:?})");
        }

        is_legal
    }

    /// Clear ponder state (internal helper)
    pub(crate) fn clear_ponder_state(&mut self) {
        self.ponder_state = PonderState {
            is_pondering: false,
            ponder_start: None,
        };
        debug!("Ponder state cleared");
    }

    /// Handle new game notification
    ///
    /// Clears the current position and ponder state to prepare for a new game.
    pub fn new_game(&mut self) {
        self.position = None;
        self.clear_ponder_state();
        info!("New game started - position and ponder state cleared");
    }

    /// Clear position (for testing position recovery)
    #[cfg(test)]
    pub fn clear_position(&mut self) {
        self.position = None;
    }
}
