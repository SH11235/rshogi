//! 合法手生成モジュール
//!
//! 局面から合法手を生成する。
//!
//! - `MoveList`: 指し手リスト
//! - `generate_*`: 各種手生成関数
//! - 王手回避手生成

mod generator;
mod movelist;
mod types;

pub use generator::{generate_all, generate_evasions, generate_legal, generate_non_evasions};
pub use movelist::MoveList;
pub use types::{ExtMove, GenType, MAX_MOVES};
