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
//!

// Phase 1: 基本型
pub mod types;

// Phase 2: 盤面表現
pub mod bitboard;
pub mod position;

// Phase 3: 合法手生成
pub mod movegen;

// Phase 4: NNUE評価
pub mod nnue;

// Phase 5: 置換表
pub mod tt;

// Phase 6-8: 探索
pub mod search;

// Phase 7: 手の順序付け
pub mod movepick;

// Phase 9: 時間管理
pub mod time;
