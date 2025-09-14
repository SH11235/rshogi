//! Error types for NNUE evaluation
//!
//! Provides error handling for NNUE operations

use super::weights::{SingleWeightsError, WeightsError};
use crate::{Color, Square};

/// NNUE-specific errors
#[derive(thiserror::Error, Debug)]
pub enum NNUEError {
    /// King not found for a specific color
    #[error("King not found for {0:?}")]
    KingNotFound(Color),

    /// Empty accumulator stack
    #[error("Empty accumulator stack")]
    EmptyAccumulatorStack,

    /// Invalid piece at a square
    #[error("Invalid piece at {0:?}")]
    InvalidPiece(Square),

    /// Invalid move for differential update
    #[error("Invalid move: {0}")]
    InvalidMove(String),

    /// File I/O error
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Classic/weights loader error (typed)
    #[error(transparent)]
    Weights(#[from] WeightsError),

    /// SINGLE weights loader error (typed)
    #[error(transparent)]
    SingleWeights(#[from] SingleWeightsError),

    /// Weight dimension mismatch
    #[error("Weight dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
}

/// Result type for NNUE operations
pub type NNUEResult<T> = Result<T, NNUEError>;
