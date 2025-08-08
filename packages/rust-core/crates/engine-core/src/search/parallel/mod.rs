//! Parallel search implementation using Lazy SMP
//!
//! This module implements a parallel search algorithm based on Lazy SMP (Symmetric MultiProcessing).
//! Each thread searches the same position from different depths to reduce duplicate work.

pub mod search_thread;
pub mod shared;
pub mod stop_test;

// New implementation
mod simple_parallel;

// Re-export with the old name for compatibility
pub use simple_parallel::SimpleParallelSearcher as ParallelSearcher;
pub use search_thread::SearchThread;
pub use shared::{SharedHistory, SharedSearchState};

// Keep DuplicationStats for compatibility (even though new implementation doesn't use it)
pub use parallel_searcher_old::DuplicationStats;
