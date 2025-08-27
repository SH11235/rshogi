//! Unified search engine with compile-time feature configuration
//!
//! This module implements a single search engine that can be configured
//! at compile time to use different features, eliminating runtime overhead.

pub mod aspiration;
pub mod builder;
pub mod context;
pub mod core;
pub mod ordering;
pub mod pruning;
pub mod time_management;
pub mod tt_operations;

#[cfg(test)]
mod see_filter_test;

#[cfg(test)]
mod tests;

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        adaptive_prefetcher::AdaptivePrefetcher,
        constants::SEARCH_INF,
        history::{CounterMoveHistory, History},
        parallel::shared::DuplicationStats,
        types::{NodeType, SearchStack},
        SearchLimits, SearchResult, SearchStats, TranspositionTable,
    },
    shogi::{Move, Position},
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use self::aspiration::AspirationWindow;
pub use self::tt_operations::TTOperations;

/// Unified searcher with compile-time feature configuration
///
/// # Type Parameters
/// - `E`: The evaluator type (e.g., MaterialEvaluator, NnueEvaluator)
/// - `USE_TT`: Whether to use transposition table
/// - `USE_PRUNING`: Whether to use advanced pruning techniques
///
/// # Examples
/// ```
/// use engine_core::search::unified::UnifiedSearcher;
/// use engine_core::evaluation::evaluate::MaterialEvaluator;
/// use engine_core::evaluation::nnue::NNUEEvaluator;
///
/// // Basic searcher with minimal features
/// type BasicSearcher = UnifiedSearcher<MaterialEvaluator, true, false>;
///
/// // Enhanced searcher with all features
/// type EnhancedSearcher = UnifiedSearcher<NNUEEvaluator, true, true>;
/// ```
pub struct UnifiedSearcher<E, const USE_TT: bool = true, const USE_PRUNING: bool = true>
where
    E: Evaluator + Send + Sync + 'static,
{
    /// The evaluation function (internally Arc-wrapped for efficient sharing)
    evaluator: Arc<E>,

    /// Transposition table (conditionally compiled)
    /// Wrapped in Arc for sharing between parallel searchers
    tt: Option<Arc<TranspositionTable>>,

    /// Move ordering history (shared with move ordering)
    history: Arc<Mutex<History>>,

    /// Move ordering module
    ordering: ordering::MoveOrdering,

    /// Principal variation table (legacy; retained for tests). Not used by core PV assembly.
    pv_table: core::PVTable,

    /// Search statistics
    stats: SearchStats,

    /// Search context
    context: context::SearchContext,

    /// Time manager reference for ponder hit handling
    time_manager: Option<Arc<crate::time_management::TimeManager>>,

    /// Search stack for tracking state at each ply
    search_stack: Vec<SearchStack>,

    /// Aspiration window manager
    aspiration_window: AspirationWindow,

    /// Runtime flag to disable prefetching (for benchmarking)
    pub(crate) disable_prefetch: bool,

    /// Adaptive prefetcher for TT (conditionally compiled)
    pub(crate) adaptive_prefetcher: Option<AdaptivePrefetcher>,

    /// Duplication statistics for parallel search (optional)
    duplication_stats: Option<Arc<DuplicationStats>>,

    /// Previous iteration's PV for move ordering
    previous_pv: Vec<Move>,

    /// Previous root position hash (to detect position changes)
    prev_root_hash: Option<u64>,
}

impl<E, const USE_TT: bool, const USE_PRUNING: bool> UnifiedSearcher<E, USE_TT, USE_PRUNING>
where
    E: Evaluator + Send + Sync + 'static,
{
    /// Expose TT handle for auxiliary queries (e.g., ponder extraction)
    pub fn tt_handle(&self) -> Option<Arc<TranspositionTable>> {
        self.tt.clone()
    }
    /// Disable prefetching (for benchmarking TTOnly mode)
    pub fn set_disable_prefetch(&mut self, disable: bool) {
        self.disable_prefetch = disable;
    }

    /// Set duplication statistics for parallel search
    pub fn set_duplication_stats(&mut self, stats: Arc<DuplicationStats>) {
        self.duplication_stats = Some(stats);
    }

    /// Evaluate the current position
    pub fn evaluate(&self, pos: &Position) -> i32 {
        self.evaluator.evaluate(pos)
    }

    /// Main search entry point
    pub fn search(&mut self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        self.search_with_options(pos, limits, true)
    }

    /// Search with options to control state reset (for parallel search)
    pub fn search_with_options(
        &mut self,
        pos: &mut Position,
        limits: SearchLimits,
        reset_stats: bool,
    ) -> SearchResult {
        // Reset search state
        if reset_stats {
            self.stats = SearchStats::default();
        }
        self.context.reset();
        self.pv_table.clear();
        self.aspiration_window.clear();

        // Clear previous PV if starting a new search from a different position
        if self.prev_root_hash != Some(pos.zobrist_hash) {
            log::debug!("Root position changed, clearing previous PV");
            self.previous_pv.clear();
            self.prev_root_hash = Some(pos.zobrist_hash);
        }

        let start_time = Instant::now();

        // Create TimeManager if needed
        self.time_manager =
            time_management::create_time_manager(&limits, pos.side_to_move, pos.ply, pos);

        // Get actual depth limit from limits (not from context which defaults to 127)
        let max_depth = limits.depth.unwrap_or(127);

        // Initialize search context with limits
        self.context.set_limits(limits);

        // Iterative deepening
        let mut best_move = None;
        let mut best_score: i32 = -SEARCH_INF; // Initialize to worst possible score
        let mut best_node_type = NodeType::Exact;
        let mut depth = 1;

        while depth <= max_depth && !self.context.should_stop() {
            // Clear all PV lines at the start of each iteration
            self.pv_table.clear_all();

            // Set current depth for logging
            self.context.set_current_depth(depth);

            // Process events including ponder hit
            self.context.process_events(&self.time_manager);

            // Check time limits via TimeManager (skip for depth 1 to ensure at least 1 ply)
            if depth > 1 && self.context.check_time_limit(self.stats.nodes, &self.time_manager) {
                break;
            }

            // Set up aspiration window for depth > 1
            let (mut alpha, mut beta) =
                self.aspiration_window.get_initial_bounds(depth, best_score);

            // Search with aspiration window
            let mut score;
            let mut pv;
            let mut aspiration_retries = 0;
            #[allow(unused_assignments)]
            let mut final_node_type = NodeType::Exact; // Default, will be updated in loop

            loop {
                // Search at current depth with window
                let result = self.search_root_with_window(pos, depth, alpha, beta);
                score = result.0;
                pv = result.1;

                // Check if score is within window
                if score > alpha && score < beta {
                    // Success - update statistics
                    if aspiration_retries == 0 {
                        self.stats.aspiration_hits =
                            Some(self.stats.aspiration_hits.unwrap_or(0) + 1);
                    }
                    final_node_type = NodeType::Exact;
                    break;
                }

                // Determine node type based on score vs bounds
                if score <= alpha {
                    final_node_type = NodeType::UpperBound;
                } else {
                    // score >= beta
                    final_node_type = NodeType::LowerBound;
                }

                // Aspiration window fail - need to re-search
                self.stats.aspiration_failures =
                    Some(self.stats.aspiration_failures.unwrap_or(0) + 1);
                self.stats.re_searches = Some(self.stats.re_searches.unwrap_or(0) + 1);

                // Check retry limit
                if self.aspiration_window.should_stop_retries(aspiration_retries) {
                    log::debug!("Aspiration window retry limit reached at depth {depth}");
                    break;
                }

                // Expand window based on how far we failed outside the bounds
                let (new_alpha, new_beta) =
                    self.aspiration_window.expand_window(score, alpha, beta, best_score);
                alpha = new_alpha;
                beta = new_beta;

                aspiration_retries += 1;

                // Check for timeout during re-search
                if self.context.should_stop() {
                    // Note: final_node_type is already set based on the last search result
                    // This ensures we use the evaluation from the interrupted search
                    break;
                }
            }

            // Always update results if we have a valid pv, even if stopping
            if !pv.is_empty() {
                best_score = score;
                best_move = Some(pv[0]);
                best_node_type = final_node_type;
                // Legacy triangular PVTable is not used for core PV assembly.
                // Keep update for backwards compatibility where tests may read it.
                self.pv_table.update_from_line(&pv);

                // Update statistics
                self.stats.depth = depth;
                self.stats.pv = pv.clone();

                // Update score history for volatility calculation
                self.aspiration_window.update_score(score, best_node_type);

                // Try to reconstruct PV from TT if we have TT enabled
                // Enabled to improve PV completeness and ponder extraction
                const ENABLE_TT_PV_RECONSTRUCTION: bool = true;

                if USE_TT && ENABLE_TT_PV_RECONSTRUCTION {
                    if let Some(ref tt) = self.tt {
                        // Clone position to avoid modifying the original
                        let mut temp_pos = pos.clone();
                        let tt_pv = tt.reconstruct_pv_from_tt(&mut temp_pos, depth);

                        // Use TT PV if it's longer or if triangular PV is incomplete
                        if !tt_pv.is_empty()
                            && (tt_pv.len() > self.stats.pv.len()
                                || (self.stats.pv.len() <= 1 && tt_pv.len() > 1))
                        {
                            log::debug!(
                                "Using TT PV (length: {}) instead of triangular PV (length: {})",
                                tt_pv.len(),
                                self.stats.pv.len()
                            );

                            // Validate that TT PV starts with the same best move
                            if tt_pv[0] == self.stats.pv[0] {
                                // Additional validation: check that TT PV is legal
                                let mut validation_pos = pos.clone();
                                let mut valid = true;

                                for (i, &mv) in tt_pv.iter().enumerate() {
                                    if !validation_pos.is_legal_move(mv) {
                                        log::warn!(
                                            "TT PV contains illegal move at ply {}: {}",
                                            i,
                                            crate::usi::move_to_usi(&mv)
                                        );
                                        valid = false;
                                        break;
                                    }
                                    let _ = validation_pos.do_move(mv);
                                }

                                if valid {
                                    self.stats.pv = tt_pv;
                                } else {
                                    log::warn!("TT PV validation failed, keeping triangular PV");
                                }
                            } else {
                                log::warn!(
                                    "TT PV starts with different move: {} vs {}",
                                    crate::usi::move_to_usi(&tt_pv[0]),
                                    crate::usi::move_to_usi(&self.stats.pv[0])
                                );
                            }
                        }
                    }
                }
            }

            // Call info callback if not stopped
            if !self.context.should_stop() {
                if let Some(callback) = self.context.info_callback() {
                    // Create snapshot of PV from stack-based PV at root
                    let pv_snapshot: Vec<Move> = if !self.search_stack.is_empty() {
                        self.search_stack[0].pv_line.to_vec()
                    } else {
                        Vec::new()
                    };

                    // Validate PV snapshot
                    if !pv_snapshot.is_empty() {
                        core::pv_local_sanity(pos, &pv_snapshot);
                    }

                    // Use snapshot for callback (immutable copy)
                    callback(
                        depth,
                        score,
                        self.stats.nodes,
                        self.context.elapsed(),
                        &pv_snapshot,
                        final_node_type,
                    );
                }

                // Save PV for next iteration's move ordering
                self.previous_pv = self.stats.pv.clone();
            }

            depth += 1;
        }

        self.stats.elapsed = start_time.elapsed();

        SearchResult {
            best_move,
            score: best_score,
            stats: self.stats.clone(),
            node_type: best_node_type,
            stop_info: None, // TODO: Will be populated in Phase 2
        }
    }

    /// Search from the root position with aspiration window
    fn search_root_with_window(
        &mut self,
        pos: &mut Position,
        depth: u8,
        alpha: i32,
        beta: i32,
    ) -> (i32, Vec<Move>) {
        // Clone previous PV to avoid borrow checker issues
        let previous_pv = self.previous_pv.clone();
        // Implementation will be added in core module
        core::search_root_with_window(self, pos, depth, alpha, beta, &previous_pv)
    }

    /// Get current node count
    pub fn nodes(&self) -> u64 {
        self.stats.nodes
    }

    /// Get principal variation
    pub fn principal_variation(&self) -> &[Move] {
        if !self.search_stack.is_empty() {
            &self.search_stack[0].pv_line
        } else {
            &[]
        }
    }

    /// Get current search depth
    pub fn current_depth(&self) -> u8 {
        self.stats.depth
    }

    /// Enable or disable TT prefetching
    pub fn set_prefetch_enabled(&mut self, enabled: bool) {
        self.disable_prefetch = !enabled;
    }

    /// Set history table (for parallel search)
    pub fn set_history(&mut self, history: History) {
        if let Ok(mut h) = self.history.lock() {
            *h = history;
        }
    }

    /// Get history table (for parallel search)
    pub fn get_history(&self) -> History {
        if let Ok(h) = self.history.lock() {
            h.clone()
        } else {
            History::new()
        }
    }

    /// Set counter moves (for parallel search)
    pub fn set_counter_moves(&mut self, counter_moves: CounterMoveHistory) {
        if let Ok(mut h) = self.history.lock() {
            h.counter_moves = counter_moves;
        }
    }

    /// Get counter moves (for parallel search)
    pub fn get_counter_moves(&self) -> CounterMoveHistory {
        if let Ok(h) = self.history.lock() {
            h.counter_moves.clone()
        } else {
            CounterMoveHistory::new()
        }
    }
}

/// Type aliases for common configurations
pub type BasicSearcher =
    UnifiedSearcher<crate::evaluation::evaluate::MaterialEvaluator, true, false>;
pub type EnhancedSearcher<E> = UnifiedSearcher<E, true, true>;
