//! 合法手生成モジュール
//!
//! 局面から pseudo-legal 手および合法手を生成する。
//!
//! - `GenType`: 探索で使用する生成モード種別
//! - `MoveList`: 固定長バッファを使った指し手リスト
//! - `generate_non_evasions` / `generate_evasions` / `generate_all`: 王手の有無に応じた pseudo-legal 手生成
//! - `generate_legal`: `Position::is_legal` でフィルタした完全合法手生成
//!
//! `generate_non_evasions` は「王手がかかっていない局面」でのみ、
//! `generate_evasions` は「王手がかかっている局面」でのみ呼び出すことを前提とする。

mod generator;
mod movelist;
mod types;

pub use generator::{
    generate_all, generate_evasions, generate_legal, generate_non_evasions, generate_with_type,
};
pub use movelist::MoveList;
pub use types::{ExtMove, GenType, MAX_MOVES};
