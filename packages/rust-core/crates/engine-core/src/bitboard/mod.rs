//! ビットボードモジュール
//!
//! 81マスの盤面を128bitで表現し、高速なビット演算を提供する。
//!
//! - `Bitboard`: 128bit盤面表現
//! - 利き計算テーブル
//! - 飛び駒の利き計算

mod core;
mod sliders;
mod tables;

pub use core::Bitboard;
pub use core::BitboardIter;
pub use sliders::*;
pub use tables::*;
