mod driver;
pub mod ordering;
mod profile;
mod pruning;
mod pv_extract;
mod pvs;
mod qsearch;
mod root_verify;

// 診断モジュールは debugビルドとdiagnostics機能、さらにテストビルドでも有効化
#[cfg(any(test, debug_assertions, feature = "diagnostics"))]
pub mod diagnostics;

pub use driver::ClassicBackend;
#[cfg(any(test, feature = "bench-move-picker"))]
pub use ordering::{Heuristics, MovePicker};
pub use profile::{PruneToggles, SearchProfile};

#[cfg(test)]
mod tests;
