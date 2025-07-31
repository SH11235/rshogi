//! Unified search limits for both basic and enhanced search

use crate::time_management::{TimeControl, TimeParameters};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use super::constants::DEFAULT_SEARCH_DEPTH;
use super::types::InfoCallback;

/// Unified search limits combining time control with other constraints
pub struct SearchLimits {
    pub time_control: TimeControl,
    pub moves_to_go: Option<u32>,
    pub depth: Option<u32>,
    pub nodes: Option<u64>,
    pub time_parameters: Option<TimeParameters>,
    /// Stop flag for interrupting search (temporarily kept for compatibility)
    pub stop_flag: Option<Arc<AtomicBool>>,
    /// Info callback for search progress (temporarily kept for compatibility)
    pub info_callback: Option<InfoCallback>,
    /// Ponder hit flag for converting ponder search to normal search
    pub ponder_hit_flag: Option<Arc<AtomicBool>>,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            time_control: TimeControl::Infinite,
            moves_to_go: None,
            depth: None,
            nodes: None,
            time_parameters: None,
            stop_flag: None,
            info_callback: None,
            ponder_hit_flag: None,
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
            Some(d) if d > 255 => {
                log::warn!("Search depth {d} exceeds u8 maximum (255), clamping to 255");
                255
            }
            Some(d) => d as u8,
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
    depth: Option<u32>,
    nodes: Option<u64>,
    time_parameters: Option<TimeParameters>,
    stop_flag: Option<Arc<AtomicBool>>,
    info_callback: Option<InfoCallback>,
    ponder_hit_flag: Option<Arc<AtomicBool>>,
}

impl Default for SearchLimitsBuilder {
    fn default() -> Self {
        Self {
            time_control: TimeControl::Infinite,
            moves_to_go: None,
            depth: None,
            nodes: None,
            time_parameters: None,
            stop_flag: None,
            info_callback: None,
            ponder_hit_flag: None,
        }
    }
}

impl SearchLimitsBuilder {
    /// Set search depth
    ///
    /// This sets a maximum depth for the search. Can be combined with time controls.
    /// When both depth and time limits are set, the search stops at whichever is reached first.
    pub fn depth(mut self, depth: u32) -> Self {
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

    /// Set Ponder mode (legacy - loses time control information)
    pub fn ponder(mut self) -> Self {
        // Create a dummy inner time control for backward compatibility
        let inner = Box::new(TimeControl::Infinite);
        self.time_control = TimeControl::Ponder(inner);
        self
    }

    /// Set Ponder mode with inner time control
    /// This preserves the existing time control settings for use after ponderhit
    pub fn ponder_with_inner(mut self) -> Self {
        // Take the current time control and wrap it in Ponder
        let inner = Box::new(self.time_control.clone());
        self.time_control = TimeControl::Ponder(inner);
        self
    }

    /// Set Infinite time control
    pub fn infinite(mut self) -> Self {
        self.time_control = TimeControl::Infinite;
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

    /// Set time parameters
    pub fn time_parameters(mut self, params: TimeParameters) -> Self {
        self.time_parameters = Some(params);
        self
    }

    /// Set stop flag
    pub fn stop_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.stop_flag = Some(flag);
        self
    }

    /// Set info callback
    pub fn info_callback(mut self, callback: InfoCallback) -> Self {
        self.info_callback = Some(callback);
        self
    }

    /// Set ponder hit flag
    pub fn ponder_hit_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.ponder_hit_flag = Some(flag);
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
            time_parameters: self.time_parameters,
            stop_flag: self.stop_flag,
            info_callback: self.info_callback,
            ponder_hit_flag: self.ponder_hit_flag,
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
        SearchLimits {
            time_control: tm.time_control,
            moves_to_go: tm.moves_to_go,
            depth: tm.depth,
            nodes: tm.nodes,
            time_parameters: tm.time_parameters,
            stop_flag: None,
            info_callback: None,
            ponder_hit_flag: None,
        }
    }
}

/// Convert to time_management TimeLimits
impl From<SearchLimits> for crate::time_management::TimeLimits {
    fn from(unified: SearchLimits) -> Self {
        crate::time_management::TimeLimits {
            time_control: unified.time_control,
            moves_to_go: unified.moves_to_go,
            depth: unified.depth,
            nodes: unified.nodes,
            time_parameters: unified.time_parameters,
        }
    }
}

/// Manual Clone implementation for SearchLimits
///
/// Note: `info_callback` is not cloneable and will always be `None` in the cloned instance.
/// This is by design as function pointers cannot be cloned. Users who need to preserve
/// the callback should set it explicitly on the cloned instance.
impl Clone for SearchLimits {
    fn clone(&self) -> Self {
        Self {
            time_control: self.time_control.clone(),
            moves_to_go: self.moves_to_go,
            depth: self.depth,
            nodes: self.nodes,
            time_parameters: self.time_parameters,
            stop_flag: self.stop_flag.clone(),
            info_callback: None, // Cannot clone function pointers
            ponder_hit_flag: self.ponder_hit_flag.clone(),
        }
    }
}

/// Manual Debug implementation for SearchLimits
///
/// Shows whether `stop_flag` and `info_callback` are present (Some/None)
/// without displaying their actual values for cleaner output.
impl std::fmt::Debug for SearchLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchLimits")
            .field("time_control", &self.time_control)
            .field("moves_to_go", &self.moves_to_go)
            .field("depth", &self.depth)
            .field("nodes", &self.nodes)
            .field("time_parameters", &self.time_parameters)
            .field("stop_flag", &self.stop_flag.is_some())
            .field("info_callback", &self.info_callback.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
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
}
