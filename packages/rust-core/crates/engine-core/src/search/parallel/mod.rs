//! Parallel search implementation using Lazy SMP
//!
//! This module implements a parallel search algorithm based on Lazy SMP (Symmetric MultiProcessing).
//! Each thread searches the same position from different depths to reduce duplicate work.

pub mod lazy_smp;
pub mod search_thread;
pub mod shared;

mod parallel_searcher;
#[cfg(test)]
mod tests;
mod time_manager;
mod work_queue;
mod worker;

pub use lazy_smp::LazySmpSearcher;
pub use parallel_searcher::ParallelSearcher;
pub use search_thread::SearchThread;
pub use shared::{SharedHistory, SharedSearchState};
#[cfg(feature = "ybwc")]
pub use shared::{SplitPoint, SplitPointManager};
