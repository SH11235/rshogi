//! Error types for the shogi engine.
//!
//! This module defines error types that can occur during engine operations,
//! including search failures, timeouts, and invalid states.

use std::error::Error;
use std::fmt;

/// Engine error types for better error handling
#[derive(Debug)]
pub enum EngineError {
    /// No legal moves available (checkmate or stalemate)
    NoLegalMoves,

    /// Engine is not available or in invalid state
    EngineNotAvailable(String),

    /// Operation timed out
    Timeout,

    /// Position was corrupted during search
    PositionCorrupted,

    /// Other errors
    Other(anyhow::Error),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::NoLegalMoves => write!(f, "No legal moves available"),
            EngineError::EngineNotAvailable(msg) => write!(f, "Engine not available: {msg}"),
            EngineError::Timeout => write!(f, "Operation timed out"),
            EngineError::PositionCorrupted => write!(f, "Position was corrupted during search"),
            EngineError::Other(e) => write!(f, "Other error: {e}"),
        }
    }
}

impl Error for EngineError {}

impl From<anyhow::Error> for EngineError {
    fn from(e: anyhow::Error) -> Self {
        EngineError::Other(e)
    }
}
