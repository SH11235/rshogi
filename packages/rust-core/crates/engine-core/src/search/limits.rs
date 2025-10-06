//! Unified search limits for both basic and enhanced search

use crate::search::parallel::StopController;
use crate::time_management::{TimeControl, TimeManager, TimeParameters};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::constants::DEFAULT_SEARCH_DEPTH;
use super::types::{InfoStringCallback, IterationCallback};

/// Unified search limits combining time control with other constraints
pub struct SearchLimits {
    pub time_control: TimeControl,
    pub moves_to_go: Option<u32>,
    pub depth: Option<u8>,
    pub nodes: Option<u64>,
    pub qnodes_limit: Option<u64>,
    pub time_parameters: Option<TimeParameters>,
    pub random_time_ms: Option<u64>,
    /// Session ID for OOB (out-of-band) finalize coordination
    /// This must match the Engine's session_id for proper snapshot reception
    /// Default: 0 (tests and legacy code), should be set by Engine::start_search()
    pub session_id: u64,
    /// Wall-clock instant when search started (used for diagnostics / elapsed derivations)
    pub start_time: Instant,
    /// Optional panic time scale for extending soft deadlines after aspiration failures etc.
    pub panic_time_scale: Option<f64>,
    /// Optional contempt value in centipawns (positive favors side to move)
    pub contempt: Option<i32>,
    /// Whether this search is running in ponder mode (go ponder)
    pub is_ponder: bool,
    /// Stop flag for interrupting search (temporarily kept for compatibility)
    pub stop_flag: Option<Arc<AtomicBool>>,
    /// Info callback for search progress (temporarily kept for compatibility)
    pub info_callback: Option<crate::search::api::InfoEventCallback>,
    /// Callback for textual diagnostics routed as `info string`
    pub info_string_callback: Option<InfoStringCallback>,
    /// Iteration callback for committed iteration results
    pub iteration_callback: Option<IterationCallback>,
    /// Ponder hit flag for converting ponder search to normal search
    pub ponder_hit_flag: Option<Arc<AtomicBool>>,
    /// Internal: Shared qnodes counter for parallel search
    /// This is set by ParallelSearcher and not exposed in the builder
    #[doc(hidden)]
    pub qnodes_counter: Option<Arc<AtomicU64>>,
    /// Optional jitter seed for helper threads (parallel search)
    pub root_jitter_seed: Option<u64>,
    /// Whether heuristics snapshots should be retained for diagnostics
    pub store_heuristics: bool,
    /// Skip quiescence search at depth 0 and return immediate evaluation
    /// This is useful for extremely time-constrained situations
    pub immediate_eval_at_depth_zero: bool,
    /// Number of principal variations to search (MultiPV)
    /// 1 = single PV (default), higher values enable multi-PV search
    pub multipv: u8,
    /// Enable fail-safe guard (parallel searchのみ). 既定: false
    pub enable_fail_safe: bool,
    /// Local deadlines used as a fallback when time manager / OOB finalize is unavailable
    pub fallback_deadlines: Option<FallbackDeadlines>,
    /// Time manager coordinating soft/hard limits (None during ponder/infinite)
    pub time_manager: Option<Arc<TimeManager>>,
    /// Stop controller used for OOB finalize coordination
    pub stop_controller: Option<Arc<StopController>>,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            time_control: TimeControl::Infinite,
            moves_to_go: None,
            depth: None,
            nodes: None,
            qnodes_limit: None,
            time_parameters: None,
            random_time_ms: None,
            session_id: 0, // Default for tests, should be set by Engine::start_search()
            start_time: Instant::now(),
            panic_time_scale: None,
            contempt: None,
            is_ponder: false,
            stop_flag: None,
            info_callback: None,
            info_string_callback: None,
            iteration_callback: None,
            ponder_hit_flag: None,
            qnodes_counter: None,
            root_jitter_seed: None,
            store_heuristics: false,
            immediate_eval_at_depth_zero: false,
            multipv: 1,
            enable_fail_safe: false,
            fallback_deadlines: None,
            time_manager: None,
            stop_controller: None,
        }
    }
}

impl SearchLimits {
    /// Create a new SearchLimitsBuilder
    pub fn builder() -> SearchLimitsBuilder {
        SearchLimitsBuilder::default()
    }

    /// Get time limit as Duration (for basic search compatibility)
    pub fn time_limit(&self) -> Option<Duration> {
        match &self.time_control {
            TimeControl::FixedTime { ms_per_move } => Some(Duration::from_millis(*ms_per_move)),
            TimeControl::Fischer { .. } | TimeControl::Byoyomi { .. } => {
                // For Fischer and Byoyomi, we need TimeManager to calculate actual time
                // Return None for now, enhanced search will handle properly
                None
            }
            TimeControl::FixedNodes { .. } | TimeControl::Infinite | TimeControl::Ponder(_) => None,
        }
    }

    /// Get node limit
    ///
    /// Returns the node limit for the search. If time_control is FixedNodes,
    /// that value takes precedence over the nodes field.
    pub fn node_limit(&self) -> Option<u64> {
        match &self.time_control {
            TimeControl::FixedNodes { nodes } => Some(*nodes),
            _ => self.nodes,
        }
    }

    /// Get depth limit (u8 for basic search compatibility)
    pub fn depth_limit_u8(&self) -> u8 {
        match self.depth {
            Some(d) => d,
            None => DEFAULT_SEARCH_DEPTH,
        }
    }
}

/// Builder for SearchLimits
///
/// The builder follows the "last write wins" principle for time control settings.
/// For example, calling `fixed_time_ms(1000).fixed_nodes(10000)` will result in
/// `FixedNodes` time control, overwriting the previous `FixedTime` setting.
///
/// Note: `depth` and time control settings (like `fixed_time_ms`) are independent:
/// - `depth` sets a maximum search depth
/// - Time control settings (fixed_time_ms, fixed_nodes, etc.) set time/resource limits
/// - When both are set, the search stops when EITHER limit is reached first
/// - Example: `.depth(10).fixed_time_ms(5000)` searches up to depth 10 OR 5 seconds
pub struct SearchLimitsBuilder {
    time_control: TimeControl,
    moves_to_go: Option<u32>,
    depth: Option<u8>,
    nodes: Option<u64>,
    qnodes_limit: Option<u64>,
    time_parameters: Option<TimeParameters>,
    random_time_ms: Option<u64>,
    session_id: u64,
    start_time: Instant,
    panic_time_scale: Option<f64>,
    contempt: Option<i32>,
    is_ponder: bool,
    stop_flag: Option<Arc<AtomicBool>>,
    info_callback: Option<crate::search::api::InfoEventCallback>,
    info_string_callback: Option<InfoStringCallback>,
    iteration_callback: Option<IterationCallback>,
    ponder_hit_flag: Option<Arc<AtomicBool>>,
    immediate_eval_at_depth_zero: bool,
    multipv: u8,
    enable_fail_safe: bool,
    fallback_deadlines: Option<FallbackDeadlines>,
    root_jitter_seed: Option<u64>,
    store_heuristics: bool,
}

impl Default for SearchLimitsBuilder {
    fn default() -> Self {
        Self {
            time_control: TimeControl::Infinite,
            moves_to_go: None,
            depth: None,
            nodes: None,
            qnodes_limit: None,
            time_parameters: None,
            random_time_ms: None,
            session_id: 0, // Default for tests, should be overridden by Engine
            start_time: Instant::now(),
            panic_time_scale: None,
            contempt: None,
            is_ponder: false,
            stop_flag: None,
            info_callback: None,
            info_string_callback: None,
            iteration_callback: None,
            ponder_hit_flag: None,
            immediate_eval_at_depth_zero: false,
            multipv: 1,
            enable_fail_safe: false,
            fallback_deadlines: None,
            root_jitter_seed: None,
            store_heuristics: false,
        }
    }
}

impl SearchLimitsBuilder {
    /// Set search depth
    ///
    /// This sets a maximum depth for the search. Can be combined with time controls.
    /// When both depth and time limits are set, the search stops at whichever is reached first.
    pub fn depth(mut self, depth: u8) -> Self {
        self.depth = Some(depth);
        self
    }

    /// Set fixed time per move in milliseconds
    ///
    /// This sets the time_control field but does NOT affect the depth field.
    /// Can be combined with depth limits - the search stops at whichever limit is reached first.
    pub fn fixed_time_ms(mut self, ms: u64) -> Self {
        self.time_control = TimeControl::FixedTime { ms_per_move: ms };
        self
    }

    /// Set fixed nodes per move
    ///
    /// This sets the time_control to FixedNodes, which takes precedence
    /// over the nodes field when determining the node limit.
    /// Also automatically sets the nodes field to maintain consistency.
    pub fn fixed_nodes(mut self, nodes: u64) -> Self {
        self.time_control = TimeControl::FixedNodes { nodes };
        self.nodes = Some(nodes); // Auto-sync to avoid validation errors
        self
    }

    /// Set time control
    pub fn time_control(mut self, tc: TimeControl) -> Self {
        self.time_control = tc;
        self
    }

    /// Set Fischer time control
    pub fn fischer(mut self, white_ms: u64, black_ms: u64, increment_ms: u64) -> Self {
        self.time_control = TimeControl::Fischer {
            white_ms,
            black_ms,
            increment_ms,
        };
        self
    }

    /// Set Byoyomi time control
    pub fn byoyomi(mut self, main_time_ms: u64, byoyomi_ms: u64, periods: u32) -> Self {
        self.time_control = TimeControl::Byoyomi {
            main_time_ms,
            byoyomi_ms,
            periods,
        };
        self
    }

    /// Override search start time (defaults to Instant::now() during build)
    pub fn start_time(mut self, instant: Instant) -> Self {
        self.start_time = instant;
        self
    }

    /// Set panic time scale for soft deadline extension (1.0 = no change)
    pub fn panic_time_scale(mut self, scale: f64) -> Self {
        self.panic_time_scale = Some(scale);
        self
    }

    /// Set contempt value in centipawns (positive favors side to move)
    pub fn contempt(mut self, cp: i32) -> Self {
        self.contempt = Some(cp);
        self
    }

    /// Set Ponder mode with inner time control
    /// This preserves the existing time control settings for use after ponderhit
    pub fn ponder_with_inner(mut self) -> Self {
        // Take the current time control and wrap it in Ponder
        let inner = Box::new(self.time_control.clone());
        self.time_control = TimeControl::Ponder(inner);
        self.is_ponder = true;
        self
    }

    /// Set Infinite time control
    pub fn infinite(mut self) -> Self {
        self.time_control = TimeControl::Infinite;
        self.is_ponder = false;
        self
    }

    /// Set moves to go
    pub fn moves_to_go(mut self, moves: u32) -> Self {
        self.moves_to_go = Some(moves);
        self
    }

    /// Set node limit (in addition to time control)
    ///
    /// This sets a node limit that is used when time_control is not FixedNodes.
    /// If time_control is FixedNodes, that value takes precedence.
    pub fn nodes(mut self, nodes: u64) -> Self {
        self.nodes = Some(nodes);
        self
    }

    /// Set quiescence search node limit
    ///
    /// This limits the number of nodes explored in quiescence search
    /// to prevent explosion in complex positions.
    pub fn qnodes_limit(mut self, limit: u64) -> Self {
        self.qnodes_limit = Some(limit);
        self
    }

    /// Set time parameters
    pub fn time_parameters(mut self, params: TimeParameters) -> Self {
        self.time_parameters = Some(params);
        self
    }

    /// Set random time override (go rtime)
    pub fn random_time_ms(mut self, ms: u64) -> Self {
        self.random_time_ms = Some(ms);
        self
    }

    /// Set stop flag
    pub fn stop_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.stop_flag = Some(flag);
        self
    }

    /// Set info callback
    pub fn info_callback(mut self, callback: crate::search::api::InfoEventCallback) -> Self {
        self.info_callback = Some(callback);
        self
    }

    /// Set callback for `info string` diagnostics.
    pub fn info_string_callback(mut self, callback: InfoStringCallback) -> Self {
        self.info_string_callback = Some(callback);
        self
    }

    /// Set iteration callback
    pub fn iteration_callback(mut self, callback: IterationCallback) -> Self {
        self.iteration_callback = Some(callback);
        self
    }

    /// Set ponder hit flag
    pub fn ponder_hit_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.ponder_hit_flag = Some(flag);
        self
    }

    /// Set immediate evaluation at depth 0
    ///
    /// When enabled, the search will return static evaluation immediately at depth 0
    /// instead of entering quiescence search. This is useful for extremely time-constrained
    /// situations where even quiescence search might exceed time limits.
    ///
    /// ## Recommended Usage Conditions:
    /// - Time budget is less than 100ms per move
    /// - Bullet games with less than 10 seconds remaining
    /// - Emergency situations where any legal move is better than timeout
    /// - When qnodes_limit is very low (< 1000)
    ///
    /// ## Trade-offs:
    /// - Pros: Guaranteed fast response, avoids timeout in critical situations
    /// - Cons: May miss tactical shots, reduced playing strength
    ///
    /// Note: This should be used sparingly as it significantly impacts move quality.
    pub fn immediate_eval_at_depth_zero(mut self, enable: bool) -> Self {
        self.immediate_eval_at_depth_zero = enable;
        self
    }

    /// Set MultiPV count
    ///
    /// Sets the number of principal variations to search. Value is clamped to range 1-20.
    /// Default is 1 (single PV search).
    pub fn multipv(mut self, k: u8) -> Self {
        self.multipv = k.clamp(1, 20);
        self
    }

    /// Set fallback deadlines for local deadline enforcement
    pub fn fallback_deadlines(mut self, deadlines: FallbackDeadlines) -> Self {
        self.fallback_deadlines = Some(deadlines);
        self
    }

    /// Enable/disable fail-safe guard (parallel search only)
    pub fn enable_fail_safe(mut self, enable: bool) -> Self {
        self.enable_fail_safe = enable;
        self
    }

    /// Enable or disable heuristics snapshot storage (diagnostic use only)
    pub fn store_heuristics(mut self, enable: bool) -> Self {
        self.store_heuristics = enable;
        self
    }

    /// Set session ID for OOB finalize coordination
    ///
    /// This is typically set by Engine::start_search() and must match the session ID
    /// used for OOB message passing to ensure proper snapshot reception.
    pub fn session_id(mut self, id: u64) -> Self {
        self.session_id = id;
        self
    }

    /// Build SearchLimits
    ///
    /// Validates the configuration and builds the SearchLimits.
    ///
    /// # Panics
    ///
    /// Panics if both FixedNodes time control and nodes field are set with different values.
    pub fn build(self) -> SearchLimits {
        // Validate that FixedNodes and nodes field don't conflict
        #[cfg(debug_assertions)]
        if let TimeControl::FixedNodes { nodes: fixed } = &self.time_control {
            if let Some(node_limit) = self.nodes {
                if *fixed != node_limit {
                    panic!(
                        "SearchLimitsBuilder validation failed: FixedNodes ({fixed}) and nodes field ({node_limit}) must match when both are set. \
                         Consider using only fixed_nodes() or ensuring both values are identical."
                    );
                }
            }
        }

        SearchLimits {
            time_control: self.time_control,
            moves_to_go: self.moves_to_go,
            depth: self.depth,
            nodes: self.nodes,
            qnodes_limit: self.qnodes_limit,
            time_parameters: self.time_parameters,
            random_time_ms: self.random_time_ms,
            session_id: self.session_id,
            start_time: self.start_time,
            panic_time_scale: self.panic_time_scale,
            contempt: self.contempt,
            is_ponder: self.is_ponder,
            stop_flag: self.stop_flag,
            info_callback: self.info_callback,
            info_string_callback: self.info_string_callback,
            iteration_callback: self.iteration_callback,
            ponder_hit_flag: self.ponder_hit_flag,
            qnodes_counter: None,
            root_jitter_seed: self.root_jitter_seed,
            store_heuristics: self.store_heuristics,
            immediate_eval_at_depth_zero: self.immediate_eval_at_depth_zero,
            multipv: self.multipv,
            enable_fail_safe: self.enable_fail_safe,
            fallback_deadlines: self.fallback_deadlines,
            time_manager: None,
            stop_controller: None,
        }
    }
}

// Conversion from/to basic search limits removed
// Basic search now uses unified SearchLimits directly

/// Convert from time_management TimeLimits
///
/// Note: This conversion sets `stop_flag` and `info_callback` to `None` as they are
/// not part of the time management module's responsibilities. These fields should be
/// set separately if needed for search control.
impl From<crate::time_management::TimeLimits> for SearchLimits {
    fn from(tm: crate::time_management::TimeLimits) -> Self {
        let is_ponder = matches!(tm.time_control, TimeControl::Ponder(_));

        SearchLimits {
            time_control: tm.time_control,
            moves_to_go: tm.moves_to_go,
            depth: tm.depth.map(|d| d as u8),
            nodes: tm.nodes,
            qnodes_limit: None,
            time_parameters: tm.time_parameters,
            random_time_ms: tm.random_time_ms,
            session_id: 0, // Default, should be set by Engine
            start_time: Instant::now(),
            panic_time_scale: None,
            contempt: None,
            is_ponder,
            stop_flag: None,
            info_callback: None,
            info_string_callback: None,
            iteration_callback: None,
            ponder_hit_flag: None,
            qnodes_counter: None,
            root_jitter_seed: None,
            store_heuristics: false,
            immediate_eval_at_depth_zero: false,
            multipv: 1,
            enable_fail_safe: false,
            fallback_deadlines: None,
            time_manager: None,
            stop_controller: None,
        }
    }
}

/// Convert to time_management TimeLimits
impl From<SearchLimits> for crate::time_management::TimeLimits {
    fn from(unified: SearchLimits) -> Self {
        // During Ponder, use Infinite time control (no time management)
        // The inner time control is preserved for ponderhit
        let time_control = match unified.time_control {
            crate::time_management::TimeControl::Ponder(_) => {
                log::debug!("Converting Ponder to Infinite for TimeManager (no time management during ponder)");
                crate::time_management::TimeControl::Infinite
            }
            other => {
                log::debug!("Using time control as-is: {other:?}");
                other
            }
        };

        crate::time_management::TimeLimits {
            time_control,
            moves_to_go: unified.moves_to_go,
            depth: unified.depth.map(|d| d as u32),
            nodes: unified.nodes,
            time_parameters: unified.time_parameters,
            random_time_ms: unified.random_time_ms,
        }
    }
}

/// Manual Debug implementation for SearchLimits
///
/// Shows whether `stop_flag`, `info_callback`, and `ponder_hit_flag` are present (Some/None)
/// without displaying their actual values for cleaner output.
impl std::fmt::Debug for SearchLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchLimits")
            .field("time_control", &self.time_control)
            .field("moves_to_go", &self.moves_to_go)
            .field("depth", &self.depth)
            .field("nodes", &self.nodes)
            .field("qnodes_limit", &self.qnodes_limit)
            .field("time_parameters", &self.time_parameters)
            .field("random_time_ms", &self.random_time_ms)
            .field("session_id", &self.session_id)
            .field("start_time", &self.start_time)
            .field("panic_time_scale", &self.panic_time_scale)
            .field("contempt", &self.contempt)
            .field("is_ponder", &self.is_ponder)
            .field("stop_flag", &self.stop_flag.is_some())
            .field("info_callback", &self.info_callback.is_some())
            .field("info_string_callback", &self.info_string_callback.is_some())
            .field("iteration_callback", &self.iteration_callback.is_some())
            .field("ponder_hit_flag", &self.ponder_hit_flag.is_some())
            .field("qnodes_counter", &self.qnodes_counter.is_some())
            .field("immediate_eval_at_depth_zero", &self.immediate_eval_at_depth_zero)
            .field("multipv", &self.multipv)
            .field("enable_fail_safe", &self.enable_fail_safe)
            .field("fallback_deadlines", &self.fallback_deadlines.is_some())
            .field("time_manager", &self.time_manager.is_some())
            .field("stop_controller", &self.stop_controller.is_some())
            .finish()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FallbackDeadlines {
    pub soft_deadline: Option<Instant>,
    pub hard_deadline: Instant,
    pub soft_limit_ms: u64,
    pub hard_limit_ms: u64,
}

#[cfg(test)]
mod tests {
    use crate::search::NodeType;

    use super::*;

    #[test]
    fn test_builder_basic_usage() {
        let limits = SearchLimits::builder().depth(10).fixed_time_ms(1000).nodes(50000).build();

        assert_eq!(limits.depth, Some(10));
        assert_eq!(limits.node_limit(), Some(50000));
        assert_eq!(limits.time_limit(), Some(Duration::from_secs(1)));
    }

    // test_conversion_from_basic removed - basic SearchLimits no longer exists
    // test_conversion_roundtrip removed - basic SearchLimits no longer exists

    #[test]
    fn test_fixed_nodes_time_control() {
        let limits = SearchLimits::builder().fixed_nodes(100000).depth(12).build();

        assert_eq!(limits.node_limit(), Some(100000));
        assert_eq!(limits.time_limit(), None);
        assert_eq!(limits.depth, Some(12));
    }

    #[test]
    fn test_infinite_time_control() {
        let limits = SearchLimits::builder().time_control(TimeControl::Infinite).depth(20).build();

        assert_eq!(limits.time_limit(), None);
        assert_eq!(limits.node_limit(), None);
        assert_eq!(limits.depth, Some(20));
    }

    #[test]
    fn test_fischer_time_control() {
        let limits = SearchLimits::builder().fischer(300000, 300000, 2000).depth(15).build();

        match limits.time_control {
            TimeControl::Fischer {
                white_ms,
                black_ms,
                increment_ms,
            } => {
                assert_eq!(white_ms, 300000);
                assert_eq!(black_ms, 300000);
                assert_eq!(increment_ms, 2000);
            }
            _ => panic!("Expected Fischer time control"),
        }
    }

    #[test]
    fn test_byoyomi_time_control() {
        let limits = SearchLimits::builder().byoyomi(600000, 30000, 1).build();

        match limits.time_control {
            TimeControl::Byoyomi {
                main_time_ms,
                byoyomi_ms,
                periods,
            } => {
                assert_eq!(main_time_ms, 600000);
                assert_eq!(byoyomi_ms, 30000);
                assert_eq!(periods, 1);
            }
            _ => panic!("Expected Byoyomi time control"),
        }
    }

    #[test]
    fn test_node_limit_precedence() {
        // FixedNodes takes precedence
        let limits = SearchLimits::builder().fixed_nodes(100000).build();

        assert_eq!(limits.node_limit(), Some(100000));

        // nodes field is used when not FixedNodes
        let limits2 = SearchLimits::builder().fixed_time_ms(1000).nodes(50000).build();

        assert_eq!(limits2.node_limit(), Some(50000));

        // When both are set with same value, it should be OK
        let limits3 = SearchLimits::builder()
            .fixed_nodes(100000)
            .nodes(100000) // Same value
            .build();

        assert_eq!(limits3.node_limit(), Some(100000));
    }

    // test_basic_conversion_no_nodes removed - basic SearchLimits no longer exists

    #[test]
    fn test_default_depth() {
        let limits = SearchLimits::default();
        assert_eq!(limits.depth_limit_u8(), DEFAULT_SEARCH_DEPTH);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(
        expected = "SearchLimitsBuilder validation failed: FixedNodes (100000) and nodes field (50000) must match when both are set"
    )]
    fn test_build_validation_mismatch() {
        // This should panic when FixedNodes and nodes field differ
        // Note: We manually set time_control to bypass the auto-sync in fixed_nodes()
        let mut builder = SearchLimits::builder();
        builder = builder.nodes(50000);
        builder = builder.time_control(TimeControl::FixedNodes { nodes: 100000 });
        let _limits = builder.build();
    }

    #[test]
    fn test_info_callback_cloning() {
        use crate::search::api::{InfoEvent, InfoEventCallback};
        use crate::search::types::RootLine;
        use crate::shogi::Move;
        use smallvec::SmallVec;
        use std::sync::atomic::{AtomicU64, Ordering};

        // Create a shared counter
        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();

        // Create an info callback that increments the counter
        let info_callback: InfoEventCallback = Arc::new(move |event| {
            if matches!(event, InfoEvent::PV { .. }) {
                counter_clone.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Create SearchLimits instances sharing the same callback Arc
        let callback_arc = info_callback;
        let limits1 = SearchLimits::builder().info_callback(Arc::clone(&callback_arc)).build();
        let limits2 = SearchLimits::builder().info_callback(callback_arc).build();

        // Both should have the callback
        assert!(limits1.info_callback.is_some());
        assert!(limits2.info_callback.is_some());

        // Call both callbacks and verify they share the same counter
        let make_line = |depth: u32, nodes: Option<u64>, idx: u8| RootLine {
            multipv_index: idx,
            root_move: Move::null(),
            score_internal: 0,
            score_cp: 0,
            bound: NodeType::Exact,
            depth,
            seldepth: Some(depth as u8),
            pv: SmallVec::new(),
            nodes,
            time_ms: Some(1),
            nps: Some(1),
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        };

        if let Some(cb1) = &limits1.info_callback {
            cb1(InfoEvent::PV {
                line: Arc::new(make_line(1, Some(100), 1)),
            });
        }
        if let Some(cb2) = &limits2.info_callback {
            cb2(InfoEvent::PV {
                line: Arc::new(make_line(2, Some(200), 2)),
            });
        }

        // Both callbacks should have incremented the same counter
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_debug_output_includes_ponder_hit_flag() {
        use std::sync::atomic::AtomicBool;

        // Test without ponder_hit_flag
        let limits_without = SearchLimits::builder().depth(10).build();
        let debug_str_without = format!("{limits_without:?}");
        assert!(debug_str_without.contains("ponder_hit_flag: false"));

        // Test with ponder_hit_flag
        let mut limits_with = SearchLimits::builder().depth(10).build();
        limits_with.ponder_hit_flag = Some(Arc::new(AtomicBool::new(false)));
        let debug_str_with = format!("{limits_with:?}");
        assert!(debug_str_with.contains("ponder_hit_flag: true"));
    }

    #[test]
    fn test_multipv_builder() {
        // Test basic usage
        let limits = SearchLimits::builder().multipv(5).build();
        assert_eq!(limits.multipv, 5);

        // Test clamping to max 20
        let limits_clamped = SearchLimits::builder().multipv(30).build();
        assert_eq!(limits_clamped.multipv, 20);

        // Test clamping to min 1
        let limits_min = SearchLimits::builder().multipv(0).build();
        assert_eq!(limits_min.multipv, 1);

        // Test default value is 1
        let limits_default = SearchLimits::builder().build();
        assert_eq!(limits_default.multipv, 1);
    }

    #[test]
    fn test_multipv_with_other_limits() {
        let limits = SearchLimits::builder()
            .depth(10)
            .fixed_time_ms(1000)
            .multipv(3)
            .nodes(50000)
            .build();

        assert_eq!(limits.depth, Some(10));
        assert_eq!(limits.multipv, 3);
        assert_eq!(limits.node_limit(), Some(50000));
        assert_eq!(limits.time_limit(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn test_multipv_repeated_builder_creates_equivalent_values() {
        let limits_a = SearchLimits::builder().multipv(7).depth(15).build();
        let limits_b = SearchLimits::builder().multipv(7).depth(15).build();

        assert_eq!(limits_a.multipv, limits_b.multipv);
        assert_eq!(limits_a.depth, limits_b.depth);
    }

    #[test]
    fn test_multipv_debug_output() {
        let limits_with = SearchLimits::builder().multipv(5).build();
        let debug_str = format!("{limits_with:?}");
        assert!(debug_str.contains("multipv: 5"));

        let limits_without = SearchLimits::builder().build();
        let debug_str_none = format!("{limits_without:?}");
        assert!(debug_str_none.contains("multipv: 1"));
    }
}
