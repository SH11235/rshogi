use crate::shogi::Color;
use std::fmt;

/// Errors that can occur during move generation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveGenError {
    /// King not found on the board
    KingNotFound(Color),
    /// Invalid square index
    InvalidSquare(usize),
    /// Position is invalid for move generation
    InvalidPosition(String),
}

impl fmt::Display for MoveGenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoveGenError::KingNotFound(color) => {
                write!(f, "King not found for {color:?}")
            }
            MoveGenError::InvalidSquare(index) => {
                write!(f, "Invalid square index: {index}")
            }
            MoveGenError::InvalidPosition(reason) => {
                write!(f, "Invalid position: {reason}")
            }
        }
    }
}

impl std::error::Error for MoveGenError {}