//! Unified search engine with compile-time feature configuration
//!
//! This module implements a single search engine that can be configured
//! at compile time to use different features, eliminating runtime overhead.

pub mod context;
pub mod core;
pub mod ordering;
pub mod pruning;

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        history::History,
        tt::{NodeType, TranspositionTable},
        types::SearchStack,
        SearchLimits, SearchResult, SearchStats,
    },
    shogi::{Move, Position},
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

/// Unified searcher with compile-time feature configuration
///
/// # Type Parameters
/// - `E`: The evaluator type (e.g., MaterialEvaluator, NnueEvaluator)
/// - `USE_TT`: Whether to use transposition table
/// - `USE_PRUNING`: Whether to use advanced pruning techniques
/// - `TT_SIZE_MB`: Transposition table size in megabytes
///
/// # Examples
/// ```
/// use engine_core::search::unified::UnifiedSearcher;
/// use engine_core::evaluation::evaluate::MaterialEvaluator;
/// use engine_core::evaluation::nnue::NNUEEvaluator;
///
/// // Basic searcher with minimal features
/// type BasicSearcher = UnifiedSearcher<MaterialEvaluator, true, false, 8>;
///
/// // Enhanced searcher with all features
/// type EnhancedSearcher = UnifiedSearcher<NNUEEvaluator, true, true, 16>;
/// ```
pub struct UnifiedSearcher<
    E,
    const USE_TT: bool = true,
    const USE_PRUNING: bool = true,
    const TT_SIZE_MB: usize = 16,
> where
    E: Evaluator + Send + Sync + 'static,
{
    /// The evaluation function (internally Arc-wrapped for efficient sharing)
    evaluator: Arc<E>,

    /// Transposition table (conditionally compiled)
    tt: Option<TranspositionTable>,

    /// Move ordering history (shared with move ordering)
    history: Arc<Mutex<History>>,

    /// Move ordering module
    ordering: ordering::MoveOrdering,

    /// Principal variation table
    pv_table: core::PVTable,

    /// Search statistics
    stats: SearchStats,

    /// Search context
    context: context::SearchContext,

    /// Time manager reference for ponder hit handling
    time_manager: Option<Arc<crate::time_management::TimeManager>>,

    /// Search stack for tracking state at each ply
    search_stack: Vec<SearchStack>,

    /// Evaluation history for each depth (for dynamic aspiration window)
    score_history: Vec<i32>,

    /// Score volatility measurement for window adjustment
    score_volatility: i32,
}

impl<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>
    UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>
where
    E: Evaluator + Send + Sync + 'static,
{
    /// Create a new unified searcher
    pub fn new(evaluator: E) -> Self {
        let history = Arc::new(Mutex::new(History::new()));
        // Pre-allocate search stack for maximum search depth
        // This is a small amount of memory (8KB) and avoids dynamic allocation during search
        let mut search_stack = Vec::with_capacity(crate::search::constants::MAX_PLY + 1);
        for ply in 0..=crate::search::constants::MAX_PLY {
            search_stack.push(SearchStack::new(ply as u16));
        }

        Self {
            evaluator: Arc::new(evaluator),
            tt: if USE_TT {
                Some(TranspositionTable::new(TT_SIZE_MB))
            } else {
                None
            },
            history: history.clone(),
            ordering: ordering::MoveOrdering::new(history),
            pv_table: core::PVTable::new(),
            stats: SearchStats::default(),
            context: context::SearchContext::new(),
            time_manager: None,
            search_stack,
            score_history: Vec::with_capacity(crate::search::constants::MAX_PLY),
            score_volatility: 0,
        }
    }

    /// Create a new unified searcher with an already Arc-wrapped evaluator
    pub fn with_arc(evaluator: Arc<E>) -> Self {
        let history = Arc::new(Mutex::new(History::new()));
        // Pre-allocate search stack for maximum search depth
        let mut search_stack = Vec::with_capacity(crate::search::constants::MAX_PLY + 1);
        for ply in 0..=crate::search::constants::MAX_PLY {
            search_stack.push(SearchStack::new(ply as u16));
        }

        Self {
            evaluator,
            tt: if USE_TT {
                Some(TranspositionTable::new(TT_SIZE_MB))
            } else {
                None
            },
            history: history.clone(),
            ordering: ordering::MoveOrdering::new(history),
            pv_table: core::PVTable::new(),
            stats: SearchStats::default(),
            context: context::SearchContext::new(),
            time_manager: None,
            search_stack,
            score_history: Vec::with_capacity(crate::search::constants::MAX_PLY),
            score_volatility: 0,
        }
    }

    /// Main search entry point
    pub fn search(&mut self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        // Reset search state
        self.stats = SearchStats::default();
        self.context.reset();
        self.pv_table.clear();
        self.score_history.clear();
        self.score_volatility = 0;

        let start_time = Instant::now();

        // Create TimeManager if needed
        use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};
        // Create TimeManager for non-infinite time controls OR when depth limit is specified
        // This enables proper search optimizations for depth-limited searches
        if !matches!(limits.time_control, TimeControl::Infinite) || limits.depth.is_some() {
            // Convert SearchLimits to TimeLimits
            let time_limits: TimeLimits = limits.clone().into();

            // Estimate game phase from position
            let game_phase = if pos.ply <= 40 {
                GamePhase::Opening
            } else if pos.ply <= 120 {
                GamePhase::MiddleGame
            } else {
                GamePhase::EndGame
            };

            let time_manager = Arc::new(TimeManager::new(
                &time_limits,
                pos.side_to_move,
                pos.ply.into(), // Convert u16 to u32
                game_phase,
            ));
            self.time_manager = Some(time_manager);
        } else {
            self.time_manager = None;
        }

        // Get actual depth limit from limits (not from context which defaults to 127)
        let max_depth = limits.depth.unwrap_or(127);

        // Initialize search context with limits
        self.context.set_limits(limits);

        // Iterative deepening
        let mut best_move = None;
        let mut best_score: i32 = 0;
        let mut depth = 1;

        while depth <= max_depth && !self.context.should_stop() {
            // Process events including ponder hit
            self.context.process_events(&self.time_manager);

            // Check time limits via TimeManager (skip for depth 1 to ensure at least 1 ply)
            if depth > 1 {
                if let Some(ref tm) = self.time_manager {
                    if tm.should_stop(self.stats.nodes) {
                        log::info!("TimeManager signaled stop after {} nodes", self.stats.nodes);
                        self.context.stop();
                        break;
                    }
                }
            }

            // Set up aspiration window for depth > 1
            let (mut alpha, mut beta) =
                if depth > 1 && best_score.abs() < crate::search::constants::MATE_SCORE {
                    // Calculate dynamic window based on score history
                    let window = self.calculate_aspiration_window(depth);
                    (best_score - window, best_score + window)
                } else {
                    // First depth or mate score - use full window
                    (-crate::search::constants::SEARCH_INF, crate::search::constants::SEARCH_INF)
                };

            // Search with aspiration window
            let mut score;
            let mut pv;
            let mut aspiration_retries = 0;

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
                    break;
                }

                // Aspiration window fail - need to re-search
                self.stats.aspiration_failures =
                    Some(self.stats.aspiration_failures.unwrap_or(0) + 1);
                self.stats.re_searches = Some(self.stats.re_searches.unwrap_or(0) + 1);

                // Check retry limit
                if aspiration_retries >= crate::search::constants::ASPIRATION_RETRY_LIMIT {
                    log::debug!("Aspiration window retry limit reached at depth {depth}");
                    break;
                }

                // Expand window gradually (1.5x expansion)
                use crate::search::constants::{
                    ASPIRATION_WINDOW_DELTA, ASPIRATION_WINDOW_EXPANSION,
                };
                if score <= alpha {
                    // Fail low - expand alpha
                    let expansion =
                        ((alpha - best_score).abs() as f32 * ASPIRATION_WINDOW_EXPANSION) as i32;
                    alpha = (alpha - expansion.max(ASPIRATION_WINDOW_DELTA))
                        .max(-crate::search::constants::SEARCH_INF);
                }
                if score >= beta {
                    // Fail high - expand beta
                    let expansion =
                        ((beta - best_score).abs() as f32 * ASPIRATION_WINDOW_EXPANSION) as i32;
                    beta = (beta + expansion.max(ASPIRATION_WINDOW_DELTA))
                        .min(crate::search::constants::SEARCH_INF);
                }

                aspiration_retries += 1;

                // Check for timeout during re-search
                if self.context.should_stop() {
                    break;
                }
            }

            // Always update results if we have a valid pv, even if stopping
            if !pv.is_empty() {
                best_score = score;
                best_move = Some(pv[0]);
                self.pv_table.update_from_line(&pv);

                // Update statistics
                self.stats.depth = depth;
                self.stats.pv = pv.clone();

                // Update score history for volatility calculation
                self.score_history.push(score);
                if self.score_history.len() > 1 {
                    self.score_volatility = self.calculate_score_volatility();
                }
            }

            // Call info callback if not stopped
            if !self.context.should_stop() {
                if let Some(callback) = self.context.info_callback() {
                    callback(depth, score, self.stats.nodes, self.context.elapsed(), &pv);
                }
            }

            depth += 1;
        }

        self.stats.elapsed = start_time.elapsed();

        SearchResult {
            best_move,
            score: best_score,
            stats: self.stats.clone(),
        }
    }

    /// Calculate dynamic aspiration window based on score history
    fn calculate_aspiration_window(&self, depth: u8) -> i32 {
        use crate::search::constants::ASPIRATION_WINDOW_INITIAL;

        // Use base window for early depths
        if depth <= 2 || self.score_history.len() < 2 {
            return ASPIRATION_WINDOW_INITIAL;
        }

        // Calculate score volatility from recent history
        let volatility = self.calculate_score_volatility();

        // Adjust window based on volatility
        // High volatility = wider window to reduce re-searches
        ASPIRATION_WINDOW_INITIAL + (volatility / 4).min(100)
    }

    /// Calculate score volatility from evaluation history
    fn calculate_score_volatility(&self) -> i32 {
        if self.score_history.len() < 2 {
            return 0;
        }

        // Calculate average deviation over recent depths
        let mut total_deviation = 0;
        let history_len = self.score_history.len();
        let start = history_len.saturating_sub(5); // Look at last 5 depths

        for i in (start + 1)..history_len {
            let diff = (self.score_history[i] - self.score_history[i - 1]).abs();
            total_deviation += diff;
        }

        // Average deviation
        let count = history_len - start - 1;
        if count > 0 {
            total_deviation / count as i32
        } else {
            0
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
        // Implementation will be added in core module
        core::search_root_with_window(self, pos, depth, alpha, beta)
    }

    /// Get current node count
    pub fn nodes(&self) -> u64 {
        self.stats.nodes
    }

    /// Get principal variation
    pub fn principal_variation(&self) -> &[Move] {
        self.pv_table.get_line(0)
    }

    /// Get current search depth
    pub fn current_depth(&self) -> u8 {
        self.stats.depth
    }

    /// Probe transposition table (compile-time optimized)
    #[inline(always)]
    pub(crate) fn probe_tt(&self, hash: u64) -> Option<crate::search::tt::TTEntry> {
        if USE_TT {
            self.tt.as_ref()?.probe(hash)
        } else {
            None
        }
    }

    /// Store in transposition table (compile-time optimized)
    #[inline(always)]
    pub(crate) fn store_tt(
        &self,
        hash: u64,
        depth: u8,
        score: i32,
        node_type: NodeType,
        best_move: Option<Move>,
    ) {
        if USE_TT {
            if let Some(ref tt) = self.tt {
                tt.store(hash, best_move, score as i16, 0, depth, node_type);
            }
        }
    }
}

/// Type aliases for common configurations
pub type BasicSearcher =
    UnifiedSearcher<crate::evaluation::evaluate::MaterialEvaluator, true, false, 8>;
pub type EnhancedSearcher<E> = UnifiedSearcher<E, true, true, 16>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::search::SearchLimitsBuilder;
    use crate::Position;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn test_unified_searcher_creation() {
        let evaluator = MaterialEvaluator;
        let searcher = UnifiedSearcher::<_, true, false, 8>::new(evaluator);
        assert_eq!(searcher.nodes(), 0);
    }

    #[test]
    fn test_compile_time_features() {
        // Test that const generic parameters work correctly
        // We can directly use the const parameters in the type
        type BasicConfig = UnifiedSearcher<MaterialEvaluator, true, false, 8>;
        type EnhancedConfig = UnifiedSearcher<MaterialEvaluator, true, true, 16>;

        // These tests verify the type system works correctly with const generics
        // The actual behavior is tested in search tests
        let basic_eval = MaterialEvaluator;
        let _basic = BasicConfig::new(basic_eval);

        let enhanced_eval = MaterialEvaluator;
        let _enhanced = EnhancedConfig::new(enhanced_eval);
    }

    #[test]
    fn test_fixed_nodes() {
        // Test FixedNodes - 時間に依存しない
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false, 8>::new(evaluator);
        let mut pos = Position::startpos();

        let limits = SearchLimitsBuilder::default().fixed_nodes(5000).build();
        let start = Instant::now();
        let result = searcher.search(&mut pos, limits);
        let elapsed = start.elapsed();

        assert!(result.best_move.is_some());
        assert!(
            result.stats.nodes <= 10000,
            "Node count {} should be reasonable (quiescence search may exceed limit)",
            result.stats.nodes
        );
        assert!(elapsed.as_secs() < 1, "Should complete within 1 second");
    }

    #[test]
    fn test_depth_limit() {
        // Test depth limit - 浅い深さで確実に終了
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false, 8>::new(evaluator);
        let mut pos = Position::startpos();

        let limits = SearchLimitsBuilder::default().depth(1).build();

        let start = Instant::now();
        let result = searcher.search(&mut pos, limits);
        let elapsed = start.elapsed();

        assert!(result.best_move.is_some());
        assert_eq!(result.stats.depth, 1);
        assert!(elapsed.as_secs() < 1, "Should complete within 1 second");
    }

    #[test]
    fn test_stop_flag_responsiveness() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false, 8>::new(evaluator);
        let mut pos = Position::startpos();
        let stop_flag = Arc::new(AtomicBool::new(false));

        // 十分なノード数を設定して、停止フラグなしでは時間がかかるようにする
        let limits = SearchLimitsBuilder::default()
            .fixed_nodes(1_000_000)
            .stop_flag(stop_flag.clone())
            .build();

        // 1ms後に停止フラグを立てる
        let stop_flag_clone = stop_flag.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(1));
            stop_flag_clone.store(true, Ordering::Relaxed);
        });

        let start = Instant::now();
        let result = searcher.search(&mut pos, limits);
        let elapsed = start.elapsed();

        assert!(result.best_move.is_some());
        assert!(
            elapsed.as_millis() < 50,
            "Search should stop within 50ms after stop flag is set, but took {}ms",
            elapsed.as_millis()
        );
    }

    #[test]
    fn test_time_manager_integration() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false, 8>::new(evaluator);
        let mut pos = Position::startpos();

        // 100msの時間制限で、深さ3に制限
        let limits = SearchLimitsBuilder::default().fixed_time_ms(100).depth(3).build();

        let start = Instant::now();
        let result = searcher.search(&mut pos, limits);
        let elapsed = start.elapsed();

        assert!(result.best_move.is_some());

        // 時間制限が効いていることを確認（マージンを持たせる）
        assert!(
            elapsed.as_millis() < 200,
            "Should stop around 100ms, but took {}ms (depth reached: {}, nodes: {})",
            elapsed.as_millis(),
            result.stats.depth,
            result.stats.nodes
        );
    }

    #[test]
    fn test_short_time_control() {
        // Test very short time controls with adaptive polling
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false, 8>::new(evaluator);
        let mut pos = Position::startpos();

        // 50msの時間制限（depth 1が完走できる程度）
        let limits = SearchLimitsBuilder::default().fixed_time_ms(50).depth(2).build();

        let start = Instant::now();
        let result = searcher.search(&mut pos, limits);
        let elapsed = start.elapsed();

        assert!(result.best_move.is_some(), "Must have best move even with short time");
        assert!(result.stats.depth >= 1, "Should complete at least depth 1");
        assert!(
            elapsed.as_millis() < 100,
            "Should stop quickly with 50ms limit, but took {}ms",
            elapsed.as_millis()
        );
    }

    #[test]
    fn test_aspiration_window_calculation() {
        let evaluator = MaterialEvaluator;
        let searcher = UnifiedSearcher::<_, true, false, 8>::new(evaluator);

        // Test base window for early depths
        let window = searcher.calculate_aspiration_window(1);
        assert_eq!(window, crate::search::constants::ASPIRATION_WINDOW_INITIAL);

        let window = searcher.calculate_aspiration_window(2);
        assert_eq!(window, crate::search::constants::ASPIRATION_WINDOW_INITIAL);
    }

    #[test]
    fn test_score_volatility_calculation() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false, 8>::new(evaluator);

        // Empty history should return 0
        assert_eq!(searcher.calculate_score_volatility(), 0);

        // Add some scores
        searcher.score_history.push(100);
        searcher.score_history.push(110);
        searcher.score_history.push(95);
        searcher.score_history.push(120);

        // Should calculate average deviation
        let volatility = searcher.calculate_score_volatility();
        assert!(volatility > 0);
        assert!(volatility < 50); // Reasonable range
    }

    #[test]
    fn test_aspiration_window_search() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, true, 8>::new(evaluator);
        let mut pos = Position::startpos();

        // Search with depth limit to test aspiration windows
        let limits = SearchLimitsBuilder::default().depth(4).build();
        let result = searcher.search(&mut pos, limits);

        assert!(result.best_move.is_some());

        // Check that aspiration window statistics were tracked
        // At depth 2 and beyond, aspiration windows should be used
        if result.stats.depth >= 2 {
            // Either hits or failures should be recorded
            let hits = result.stats.aspiration_hits.unwrap_or(0);
            let failures = result.stats.aspiration_failures.unwrap_or(0);
            assert!(hits > 0 || failures > 0, "Aspiration window should be used at depth >= 2");
        }
    }

    #[test]
    fn test_aspiration_window_with_volatile_position() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, true, 8>::new(evaluator);

        // Use a tactical position that might have score fluctuations
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let mut pos = Position::from_sfen(sfen).expect("Valid SFEN");

        let limits = SearchLimitsBuilder::default().depth(5).build();
        let result = searcher.search(&mut pos, limits);

        assert!(result.best_move.is_some());

        // Check score history was populated
        assert!(!searcher.score_history.is_empty());
        assert_eq!(searcher.score_history.len(), result.stats.depth as usize);
    }
}
