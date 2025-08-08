//! Lazy SMP parallel search implementation
//!
//! A simpler parallel search approach where each thread runs an independent search
//! with different parameters (depth offset, random seed, etc.) and only shares
//! the transposition table.

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        unified::UnifiedSearcher, SearchLimits, SearchLimitsBuilder, SearchResult, SearchStats,
        ShardedTranspositionTable,
    },
    shogi::Position,
    time_management::TimeControl,
};
use crossbeam::scope;
use log::{debug, info};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Barrier,
    },
    thread,
    time::{Duration, Instant},
};

/// Lazy SMP searcher - simple but effective parallel search
pub struct LazySmpSearcher<E: Evaluator> {
    /// Number of threads to use
    num_threads: usize,
    /// Evaluator for position evaluation
    evaluator: Arc<E>,
    /// Shared transposition table
    tt: Arc<ShardedTranspositionTable>,
}

impl<E: Evaluator + Clone + Send + Sync + 'static> LazySmpSearcher<E> {
    /// Create a new Lazy SMP searcher
    pub fn new(evaluator: E, num_threads: usize, tt_size_mb: usize) -> Self {
        Self {
            num_threads,
            evaluator: Arc::new(evaluator),
            tt: Arc::new(ShardedTranspositionTable::new(tt_size_mb)),
        }
    }

    /// Search with Lazy SMP
    pub fn search(&mut self, position: &Position, limits: SearchLimits) -> SearchResult {
        info!("Starting Lazy SMP search with {} threads", self.num_threads);
        info!("Search limits: {limits:?}");

        let should_stop = Arc::new(AtomicBool::new(false));
        let total_nodes = Arc::new(AtomicU64::new(0));

        // Create barrier for synchronized start
        let barrier = Arc::new(Barrier::new(self.num_threads + 1)); // +1 for main thread

        // Set up timer thread for time-limited searches
        if let TimeControl::FixedTime { ms_per_move } = limits.time_control {
            let timer_stop = should_stop.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(ms_per_move));
                info!("Timer expired after {ms_per_move}ms, stopping search");
                timer_stop.store(true, Ordering::Release);
            });
        }

        // Clear TT for new search (TODO: make TT clearable through Arc)
        // self.tt.clear();

        let result = scope(|s| {
            let mut handles = Vec::with_capacity(self.num_threads);

            // Spawn worker threads
            for thread_id in 0..self.num_threads {
                let position = position.clone();
                let limits = limits.clone();
                let evaluator = self.evaluator.clone();
                let tt = self.tt.clone(); // Clone the shared TT
                let should_stop = should_stop.clone();
                let total_nodes = total_nodes.clone();
                let thread_barrier = barrier.clone();

                let handle = s.spawn(move |_| {
                    debug!("Thread {thread_id} waiting at barrier");

                    // Wait for all threads to be ready
                    thread_barrier.wait();

                    debug!("Thread {thread_id} starting search");

                    // Create thread-local searcher with shared TT
                    let mut searcher = UnifiedSearcher::<E, true, true, 16>::with_shared_tt(
                        evaluator.clone(), // Use Arc directly
                        tt,                // Use the shared TT
                    );

                    let mut thread_result = SearchResult::new(None, 0, SearchStats::default());

                    // Check if this is time mode or depth mode
                    if matches!(limits.time_control, TimeControl::FixedTime { .. }) {
                        // TIME MODE: Single search call, let UnifiedSearcher handle iterative deepening
                        debug!("Thread {thread_id} in time mode");

                        // Fresh position clone for this search
                        let mut pos = position.clone();

                        // Build search limits for time mode (no depth limit)
                        let mut search_limits = SearchLimitsBuilder::default()
                            .time_control(limits.time_control.clone())
                            .build();
                        search_limits.stop_flag = Some(should_stop.clone());

                        // Single search call - UnifiedSearcher handles iterative deepening internally
                        thread_result = searcher.search(&mut pos, search_limits);
                    } else {
                        // DEPTH MODE: Manual iterative deepening with fresh position each iteration
                        debug!("Thread {thread_id} in depth mode");

                        let depth_offset = thread_id % 2; // Alternate between depths for diversity
                        let max_depth = limits.depth.unwrap_or(64);

                        // Manual iterative deepening
                        for depth in 1..=max_depth {
                            if should_stop.load(Ordering::Relaxed) {
                                break;
                            }

                            // Apply depth variation for diversity
                            let search_depth = if thread_id == 0 {
                                depth // Main thread searches exact depth
                            } else {
                                depth.saturating_add(depth_offset as u8)
                            };

                            // IMPORTANT: Fresh position clone for each iteration to avoid corruption
                            let mut pos = position.clone();

                            // Build search limits for depth mode (no time control)
                            let mut search_limits =
                                SearchLimitsBuilder::default().depth(search_depth).build();
                            search_limits.stop_flag = Some(should_stop.clone());

                            let result = searcher.search(&mut pos, search_limits);

                            if let Some(best_move) = result.best_move {
                                thread_result.best_move = Some(best_move);
                                thread_result.score = result.score;
                                thread_result.stats.depth = result.stats.depth;
                            }
                        }
                    }

                    // Update total nodes
                    let final_nodes = searcher.nodes();
                    total_nodes.fetch_add(final_nodes, Ordering::Relaxed);

                    debug!("Thread {thread_id} finished with {final_nodes} nodes");
                    thread_result
                });

                handles.push(handle);
            }

            // Start timer AFTER all threads are spawned
            barrier.wait(); // Release all threads to start simultaneously
            let search_start = Instant::now();

            // Wait for all threads and collect results
            let results: Vec<SearchResult> =
                handles.into_iter().map(|h| h.join().unwrap()).collect();

            // Select best result (from main thread or highest scoring)
            let mut best = results
                .into_iter()
                .max_by_key(|r| (r.best_move.is_some() as i32, r.score))
                .unwrap_or_else(|| SearchResult::new(None, 0, SearchStats::default()));

            // Update final stats with total nodes and elapsed time
            let total = total_nodes.load(Ordering::Relaxed);
            let elapsed = search_start.elapsed();

            best.stats.nodes = total; // Set the actual total nodes
            best.stats.elapsed = elapsed; // Set the elapsed time

            best
        })
        .unwrap();

        let final_nodes = result.stats.nodes;
        let final_elapsed = result.stats.elapsed;
        let nps = if final_elapsed.as_millis() > 0 {
            (final_nodes as u128 * 1000 / final_elapsed.as_millis()) as u64
        } else {
            0
        };

        info!(
            "Lazy SMP search complete: {} nodes in {}ms = {} nps",
            final_nodes,
            final_elapsed.as_millis(),
            nps
        );

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;

    #[test]
    fn test_lazy_smp_basic() {
        let evaluator = MaterialEvaluator;
        let mut searcher = LazySmpSearcher::new(evaluator, 2, 16);
        let position = Position::startpos();
        let limits = SearchLimitsBuilder::default().depth(4).build();

        let result = searcher.search(&position, limits);
        assert!(result.best_move.is_some());
    }
}
