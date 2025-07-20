//! Error types for NNUE evaluation
//!
//! Provides error handling for NNUE operations

use std::error::Error;
use std::fmt;

use crate::{Color, Square};

/// NNUE-specific errors
#[derive(Debug, Clone)]
pub enum NNUEError {
    /// King not found for a specific color
    KingNotFound(Color),

    /// Empty accumulator stack
    EmptyAccumulatorStack,

    /// Invalid piece at a square
    InvalidPiece(Square),

    /// Invalid move for differential update
    InvalidMove(String),

    /// File I/O error
    IoError(String),

    /// Invalid NNUE file format
    InvalidFormat(String),

    /// Weight dimension mismatch
    DimensionMismatch { expected: usize, actual: usize },
}

impl fmt::Display for NNUEError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NNUEError::KingNotFound(color) => {
                write!(f, "King not found for {color:?}")
            }
            NNUEError::EmptyAccumulatorStack => {
                write!(f, "Empty accumulator stack")
            }
            NNUEError::InvalidPiece(square) => {
                write!(f, "Invalid piece at {square:?}")
            }
            NNUEError::InvalidMove(desc) => {
                write!(f, "Invalid move: {desc}")
            }
            NNUEError::IoError(msg) => {
                write!(f, "I/O error: {msg}")
            }
            NNUEError::InvalidFormat(msg) => {
                write!(f, "Invalid NNUE format: {msg}")
            }
            NNUEError::DimensionMismatch { expected, actual } => {
                write!(f, "Weight dimension mismatch: expected {expected}, got {actual}")
            }
        }
    }
}

impl Error for NNUEError {}

/// Convert std::io::Error to NNUEError
impl From<std::io::Error> for NNUEError {
    fn from(err: std::io::Error) -> Self {
        NNUEError::IoError(err.to_string())
    }
}

/// Result type for NNUE operations
pub type NNUEResult<T> = Result<T, NNUEError>;
