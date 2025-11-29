//! 探索モジュール
//!
//! Alpha-Beta探索と各種枝刈り。
//!
//! - Iterative Deepening
//! - Alpha-Beta with PVS
//! - Aspiration Windows
//! - 静止探索（Quiescence Search）
//! - 各種枝刈り（NMP, LMR, Futility, SEE, Razoring, Singular Extension）

mod alpha_beta;
mod engine;
mod history;
mod limits;
mod movepicker;
mod time_manager;
mod time_options;
mod tt_history;
mod types;

#[cfg(test)]
mod tests;

pub use alpha_beta::*;
pub use engine::*;
pub use history::*;
pub use limits::*;
pub use movepicker::*;
pub use time_manager::*;
pub use time_options::*;
pub use tt_history::*;
pub use types::*;

/// 探索モジュールの初期化
///
/// 現状は LMR 用の reduction テーブルを初期化するだけだが、
/// 将来的に探索モジュール全体の初期化処理をまとめる窓口として使用する。
pub fn init_search_module() {
    alpha_beta::init_reductions();
}
