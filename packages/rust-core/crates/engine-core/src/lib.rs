//! # engine-core
//!
//! YaneuraOu準拠の将棋エンジンコアライブラリ。
//!
//! ## モジュール構成
//!
//! - `types`: 基本型（Color, Square, Piece, Move, Value, etc.）
//! - `bitboard`: ビットボード演算
//! - `position`: 局面表現とdo_move/undo_move
//! - `movegen`: 合法手生成
//! - `nnue`: NNUE評価関数
//! - `tt`: 置換表（Transposition Table）
//! - `search`: 探索アルゴリズム
//! - `movepick`: 手の順序付け
//! - `time`: 時間管理
//! - `mate`: 1手詰め探索
//!

pub mod types;

// 盤面表現
pub mod bitboard;
pub mod eval;
pub mod position;

// 合法手生成
pub mod movegen;

// NNUE評価
pub mod nnue;

// 置換表
pub mod tt;

//  探索
pub mod search;

// 1手詰め探索
pub mod mate;

pub use position::json_conversion;
