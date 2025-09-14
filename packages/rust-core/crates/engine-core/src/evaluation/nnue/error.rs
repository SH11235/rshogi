//! Error types for NNUE evaluation
//!
//! Provides error handling for NNUE operations

use std::error::Error;
use std::fmt;

use super::weights::WeightsError;
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

/// Convert weight-loading errors into NNUEError (for unified handling)
impl From<WeightsError> for NNUEError {
    fn from(e: WeightsError) -> Self {
        match e {
            WeightsError::Io(ioe) => NNUEError::IoError(ioe.to_string()),
            WeightsError::InvalidMagic => NNUEError::InvalidFormat("invalid magic".into()),
            WeightsError::UnsupportedVersion { found, min, max } => NNUEError::InvalidFormat(
                format!("unsupported version: {}, supported {}-{}", found, min, max),
            ),
            WeightsError::UnsupportedArchitectureV1(a)
            | WeightsError::UnsupportedArchitectureV2(a) => {
                NNUEError::InvalidFormat(format!("unsupported architecture: 0x{a:08X}"))
            }
            WeightsError::SizeTooLarge { size, max } => {
                NNUEError::InvalidFormat(format!("file too large: {} (max {})", size, max))
            }
            WeightsError::SizeMismatchV1 { expected, actual }
            | WeightsError::SizeMismatchV2 { expected, actual } => NNUEError::InvalidFormat(
                format!("size mismatch: expected {}, actual {}", expected, actual),
            ),
            WeightsError::DimsInvalid => NNUEError::InvalidFormat("invalid dims".into()),
            WeightsError::DimsInconsistent(m) => {
                NNUEError::InvalidFormat(format!("dims inconsistent: {m}"))
            }
            WeightsError::SectionTruncated(m) => {
                NNUEError::InvalidFormat(format!("section truncated: {m}"))
            }
            WeightsError::Overflow(m) => NNUEError::InvalidFormat(format!("overflow: {m}")),
        }
    }
}

/// Result type for NNUE operations
pub type NNUEResult<T> = Result<T, NNUEError>;
