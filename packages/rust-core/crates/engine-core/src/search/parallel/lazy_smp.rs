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
        Arc,
    },
    time::Instant,
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
        
        let start_time = Instant::now();
        let should_stop = Arc::new(AtomicBool::new(false));
        let total_nodes = Arc::new(AtomicU64::new(0));
        
        // Clear TT for new search (TODO: make TT clearable through Arc)
        // self.tt.clear();
        
        let result = scope(|s| {
            let mut handles = Vec::with_capacity(self.num_threads);
            
            // Spawn worker threads
            for thread_id in 0..self.num_threads {
                let position = position.clone();
                let limits = limits.clone();
                let evaluator = self.evaluator.clone();
                let should_stop = should_stop.clone();
                let total_nodes = total_nodes.clone();
                
                let handle = s.spawn(move |_| {
                    debug!("Thread {} starting", thread_id);
                    
                    // Create thread-local searcher (each with its own TT for now)
                    // TODO: Implement shared TT support in UnifiedSearcher
                    let mut searcher = UnifiedSearcher::<E, true, true, 16>::new(
                        (*evaluator).clone(),
                    );
                    
                    // Set different parameters for each thread
                    let depth_offset = thread_id % 2; // Alternate between depths
                    let mut thread_result = SearchResult::new(None, 0, SearchStats::default());
                    let mut local_position = position.clone();
                    
                    // Iterative deepening
                    for depth in 1..=limits.depth.unwrap_or(64) {
                        if should_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        
                        // Apply depth variation for diversity
                        let search_depth = if thread_id == 0 {
                            depth // Main thread searches exact depth
                        } else {
                            depth.saturating_add(depth_offset as u8)
                        };
                        
                        // Do the search with depth limit
                        let search_limits = SearchLimitsBuilder::default()
                            .depth(search_depth)
                            .build();
                        let result = searcher.search(&mut local_position, search_limits);
                        
                        if let Some(best_move) = result.best_move {
                            thread_result.best_move = Some(best_move);
                            thread_result.score = result.score;
                        }
                        
                        // Update node count
                        let nodes = searcher.nodes();
                        total_nodes.fetch_add(nodes, Ordering::Relaxed);
                        
                        // Check time limit (only main thread)
                        if thread_id == 0 {
                            let elapsed = start_time.elapsed();
                            // Check if we have a fixed time limit
                            if let TimeControl::FixedTime { ms_per_move } = limits.time_control {
                                if elapsed.as_millis() >= ms_per_move as u128 {
                                    info!("Time limit reached, stopping search");
                                    should_stop.store(true, Ordering::Relaxed);
                                    break;
                                }
                            }
                        }
                    }
                    
                    debug!("Thread {} finished with {} nodes", thread_id, searcher.nodes());
                    thread_result
                });
                
                handles.push(handle);
            }
            
            // Wait for all threads and collect results
            let results: Vec<SearchResult> = handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect();
            
            // Select best result (from main thread or highest scoring)
            results
                .into_iter()
                .max_by_key(|r| (r.best_move.is_some() as i32, r.score))
                .unwrap_or_else(|| SearchResult::new(None, 0, SearchStats::default()))
        }).unwrap();
        
        let elapsed = start_time.elapsed();
        let total_nodes = total_nodes.load(Ordering::Relaxed);
        let nps = if elapsed.as_millis() > 0 {
            (total_nodes as u128 * 1000 / elapsed.as_millis()) as u64
        } else {
            0
        };
        
        info!(
            "Lazy SMP search complete: {} nodes in {}ms = {} nps",
            total_nodes,
            elapsed.as_millis(),
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
        let limits = SearchLimitsBuilder::default()
            .depth(4)
            .build();
        
        let result = searcher.search(&position, limits);
        assert!(result.best_move.is_some());
    }
}