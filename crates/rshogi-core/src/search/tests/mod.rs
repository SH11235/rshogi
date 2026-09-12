//! 探索モジュールのテスト

mod alpha_beta;
// search_helper が wasm32 (wasm-threads なし) では configured out されるため合わせて gate する
#[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threads"))]
mod cutoff_cnt;
mod history_update;
mod multi_pv;
mod skill;
mod time_management;

mod mate1_tt;
mod probcut_abort;

mod pv_legality;
mod terminal_result;
