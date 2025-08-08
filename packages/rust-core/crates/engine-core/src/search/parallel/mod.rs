//! Parallel search implementation using Lazy SMP
//!
//! This module implements a parallel search algorithm based on Lazy SMP (Symmetric MultiProcessing).
//! Each thread searches the same position from different depths to reduce duplicate work.

pub mod search_thread;
pub mod shared;
pub mod lazy_smp;

mod parallel_searcher;

pub use parallel_searcher::ParallelSearcher;
pub use lazy_smp::LazySmpSearcher;
pub use search_thread::SearchThread;
pub use shared::{SharedHistory, SharedSearchState};
#[cfg(feature = "ybwc")]
pub use shared::{SplitPoint, SplitPointManager};
