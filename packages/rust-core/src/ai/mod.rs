//! Shogi AI Engine Module
//!
//! High-performance shogi AI implementation using alpha-beta search and NNUE evaluation

pub mod attacks;
pub mod benchmark;
pub mod board;
pub mod evaluate;
pub mod movegen;
pub mod moves;
pub mod search;
pub mod zobrist;

// Modules to be added later:
// pub mod tt;         // Transposition table

#[cfg(test)]
mod test_king_capture;

// Re-export basic types
pub use attacks::{AttackTables, Direction, ATTACK_TABLES};
pub use board::{Bitboard, Board, Color, Piece, PieceType, Position, Square};
pub use evaluate::evaluate;
pub use movegen::MoveGen;
pub use moves::{Move, MoveList};
pub use search::{SearchLimits, SearchResult, Searcher};
pub use zobrist::{ZobristHashing, ZOBRIST};
