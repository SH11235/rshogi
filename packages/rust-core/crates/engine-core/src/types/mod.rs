//! 基本型モジュール
//!
//! 将棋エンジンで使用する基本的な型を定義する。
//!
//! # 型の依存関係
//!
//! ```text
//! Color
//!   ↓
//! File, Rank
//!   ↓
//! Square
//!   ↓
//! PieceType
//!   ↓
//! Piece ← Move
//!   ↓
//! Hand
//!
//! Value, Depth, Bound, RepetitionState は独立
//! ```

mod bound;
mod color;
mod depth;
mod file;
mod hand;
mod moves;
mod piece;
mod piece_type;
mod rank;
mod repetition;
mod square;
mod value;

pub use bound::Bound;
pub use color::Color;
pub use depth::*;
pub use file::File;
pub use hand::Hand;
pub use moves::Move;
pub use piece::Piece;
pub use piece_type::PieceType;
pub use rank::Rank;
pub use repetition::RepetitionState;
pub use square::Square;
pub use value::Value;
