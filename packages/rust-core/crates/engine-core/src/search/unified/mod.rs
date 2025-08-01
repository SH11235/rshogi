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
        SearchLimits, SearchResult, SearchStats,
    },
    shogi::{Move, Position},
};
use std::{sync::Arc, time::Instant};

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
/// // Basic searcher with minimal features
/// type BasicSearcher = UnifiedSearcher<MaterialEvaluator, true, false, 8>;
///
/// // Enhanced searcher with all features
/// type EnhancedSearcher = UnifiedSearcher<NnueEvaluator, true, true, 16>;
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

    /// Move ordering history
    history: History,

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
}

impl<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>
    UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>
where
    E: Evaluator + Send + Sync + 'static,
{
    /// Create a new unified searcher
    pub fn new(evaluator: E) -> Self {
        let mut history = History::new();
        let history_ptr = &mut history as *mut History;

        Self {
            evaluator: Arc::new(evaluator),
            tt: if USE_TT {
                Some(TranspositionTable::new(TT_SIZE_MB))
            } else {
                None
            },
            history,
            ordering: ordering::MoveOrdering::new(history_ptr),
            pv_table: core::PVTable::new(),
            stats: SearchStats::default(),
            context: context::SearchContext::new(),
            time_manager: None,
        }
    }

    /// Create a new unified searcher with an already Arc-wrapped evaluator
    pub fn with_arc(evaluator: Arc<E>) -> Self {
        let mut history = History::new();
        let history_ptr = &mut history as *mut History;

        Self {
            evaluator,
            tt: if USE_TT {
                Some(TranspositionTable::new(TT_SIZE_MB))
            } else {
                None
            },
            history,
            ordering: ordering::MoveOrdering::new(history_ptr),
            pv_table: core::PVTable::new(),
            stats: SearchStats::default(),
            context: context::SearchContext::new(),
            time_manager: None,
        }
    }

    /// Main search entry point
    pub fn search(&mut self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        // Reset search state
        self.stats = SearchStats::default();
        self.context.reset();
        self.pv_table.clear();

        let start_time = Instant::now();

        // Create TimeManager if needed
        use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};
        // TODO: Performance concern - TimeControl::Infinite with depth limit
        // Currently TimeManager is not created for Infinite time control, which may
        // cause performance issues for depth-limited searches (e.g., depth 5 taking 25s).
        // Consider creating TimeManager even for Infinite to enable search optimizations.
        if !matches!(limits.time_control, TimeControl::Infinite) {
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
        let mut best_score = 0;
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

            // Search at current depth
            let (score, pv) = self.search_root(pos, depth);

            // Always update results if we have a valid pv, even if stopping
            if !pv.is_empty() {
                best_score = score;
                best_move = Some(pv[0]);
                self.pv_table.update_from_line(&pv);

                // Update statistics
                self.stats.depth = depth;
                self.stats.pv = pv.clone();
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

    /// Search from the root position
    fn search_root(&mut self, pos: &mut Position, depth: u8) -> (i32, Vec<Move>) {
        // Implementation will be added in core module
        core::search_root(self, pos, depth)
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

        // 100msの時間制限で、深さ3（より浅い深さで確実に停止）
        let limits = SearchLimitsBuilder::default().fixed_time_ms(100).depth(3).build();

        let start = Instant::now();
        let result = searcher.search(&mut pos, limits);
        let elapsed = start.elapsed();

        assert!(result.best_move.is_some());
        assert!(
            elapsed.as_millis() < 200,
            "Should stop around 100ms, but took {}ms",
            elapsed.as_millis()
        );
        // 時間制限に少し余裕を持たせる（100ms→200ms）
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
}
