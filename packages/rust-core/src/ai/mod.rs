//! Shogi AI Engine Module
//!
//! High-performance shogi AI implementation using alpha-beta search and NNUE evaluation

pub mod attacks;
pub mod benchmark;
pub mod board;
pub mod engine;
pub mod evaluate;
pub mod history;
pub mod move_picker;
pub mod movegen;
pub mod moves;
pub mod nnue;
pub mod piece_constants;
pub mod search;
pub mod search_enhanced;
pub mod tt;
pub mod zobrist;

#[cfg(test)]
mod test_move_picker_comprehensive;
#[cfg(test)]
mod test_move_picker_integration;

// Re-export basic types
pub use attacks::{AttackTables, Direction, ATTACK_TABLES};
pub use board::{Bitboard, Board, Color, Piece, PieceType, Position, Square};
pub use engine::Engine;
pub use evaluate::{evaluate, Evaluator, MaterialEvaluator};
pub use movegen::MoveGen;
pub use moves::{Move, MoveList};
pub use search::{SearchLimits, SearchResult, Searcher};
pub use zobrist::{ZobristHashing, ZOBRIST};
