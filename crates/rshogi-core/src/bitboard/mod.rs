//! ビットボードモジュール
//!
//! 81マスの盤面を128bitで表現し、高速なビット演算と利き計算を提供する。
//!
//! - `Bitboard`: 128bit盤面表現（縦型: p[0]=1-7筋, p[1]=8-9筋）
//! - 筋・段・升ごとのマスク（`FILE_BB`, `RANK_BB`, `SQUARE_BB`）
//! - 近接駒の利きテーブル（歩・桂・銀・金・玉）
//! - 遠方駒の利き計算（香・角・飛・馬・龍、`between_bb` / `line_bb` など）

mod bitboard256;
mod check_candidate;
mod core;
mod sliders;
mod step_effects;
mod tables;
mod utils;

pub use bitboard256::Bitboard256;
pub use check_candidate::check_candidate_bb;
pub use core::Bitboard;
pub use core::BitboardIter;
pub use sliders::*;
pub use step_effects::{BISHOP_STEP, ROOK_STEP};
pub use tables::*;
pub use utils::*;

/// 王手候補（check_candidate）テーブルの初期化。エンジン起動時に 1 回呼ぶこと。
/// 遠方駒の利きテーブルはコンパイル時に生成されるため、実行時の初期化は不要。
pub fn init_bitboard_tables() {
    sliders::ensure_slider_initialized();
    check_candidate::ensure_check_candidate_initialized();
}
