//! Position management functionality for the engine adapter.
//!
//! This module handles position state management, move validation,
//! and position updates from USI commands.

use anyhow::{Context, Result};
use engine_core::shogi::Position;
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

    /// Check if a USI move string is legal in the current position (engine-core util)
    pub fn is_legal_move(&self, usi_move: &str) -> bool {
        let Some(position) = &self.position else {
            warn!("Cannot check move legality: no position set");
            return false;
        };
        engine_core::util::usi_helpers::is_legal_usi_move(position, usi_move)
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
}
