//! Unified search limits for both basic and enhanced search

use crate::time_management::{TimeControl, TimeParameters};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

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
            TimeControl::FixedNodes { .. } | TimeControl::Infinite | TimeControl::Ponder => None,
        }
    }

    /// Get node limit
    pub fn node_limit(&self) -> Option<u64> {
        match &self.time_control {
            TimeControl::FixedNodes { nodes } => Some(*nodes),
            _ => self.nodes,
        }
    }

    /// Get depth limit (u8 for basic search compatibility)
    pub fn depth_limit_u8(&self) -> u8 {
        self.depth.map(|d| d.min(255) as u8).unwrap_or(6)
    }
}

/// Builder for SearchLimits
pub struct SearchLimitsBuilder {
    time_control: TimeControl,
    moves_to_go: Option<u32>,
    depth: Option<u32>,
    nodes: Option<u64>,
    time_parameters: Option<TimeParameters>,
    stop_flag: Option<Arc<AtomicBool>>,
    info_callback: Option<InfoCallback>,
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
        }
    }
}

impl SearchLimitsBuilder {
    /// Set search depth
    pub fn depth(mut self, depth: u32) -> Self {
        self.depth = Some(depth);
        self
    }

    /// Set fixed time per move in milliseconds
    pub fn fixed_time_ms(mut self, ms: u64) -> Self {
        self.time_control = TimeControl::FixedTime { ms_per_move: ms };
        self
    }

    /// Set fixed nodes per move
    pub fn fixed_nodes(mut self, nodes: u64) -> Self {
        self.time_control = TimeControl::FixedNodes { nodes };
        self
    }

    /// Set time control
    pub fn time_control(mut self, tc: TimeControl) -> Self {
        self.time_control = tc;
        self
    }

    /// Set moves to go
    pub fn moves_to_go(mut self, moves: u32) -> Self {
        self.moves_to_go = Some(moves);
        self
    }

    /// Set node limit (in addition to time control)
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

    /// Build SearchLimits
    pub fn build(self) -> SearchLimits {
        SearchLimits {
            time_control: self.time_control,
            moves_to_go: self.moves_to_go,
            depth: self.depth,
            nodes: self.nodes,
            time_parameters: self.time_parameters,
            stop_flag: self.stop_flag,
            info_callback: self.info_callback,
        }
    }
}

/// Convert from basic search limits (for compatibility)
impl From<super::search_basic::SearchLimits> for SearchLimits {
    fn from(basic: super::search_basic::SearchLimits) -> Self {
        let mut builder = SearchLimits::builder()
            .depth(basic.depth as u32)
            .nodes(basic.nodes.unwrap_or(0));

        if let Some(time) = basic.time {
            builder = builder.fixed_time_ms(time.as_millis() as u64);
        }

        if let Some(stop_flag) = basic.stop_flag {
            builder = builder.stop_flag(stop_flag);
        }

        if let Some(info_callback) = basic.info_callback {
            builder = builder.info_callback(info_callback);
        }

        builder.build()
    }
}

/// Convert to basic search limits (for compatibility)
impl From<SearchLimits> for super::search_basic::SearchLimits {
    fn from(unified: SearchLimits) -> Self {
        super::search_basic::SearchLimits {
            depth: unified.depth_limit_u8(),
            time: unified.time_limit(),
            nodes: unified.node_limit(),
            stop_flag: unified.stop_flag,
            info_callback: unified.info_callback,
        }
    }
}

/// Convert from time_management SearchLimits
impl From<crate::time_management::SearchLimits> for SearchLimits {
    fn from(tm: crate::time_management::SearchLimits) -> Self {
        SearchLimits {
            time_control: tm.time_control,
            moves_to_go: tm.moves_to_go,
            depth: tm.depth,
            nodes: tm.nodes,
            time_parameters: tm.time_parameters,
            stop_flag: None,
            info_callback: None,
        }
    }
}

/// Convert to time_management SearchLimits
impl From<SearchLimits> for crate::time_management::SearchLimits {
    fn from(unified: SearchLimits) -> Self {
        crate::time_management::SearchLimits {
            time_control: unified.time_control,
            moves_to_go: unified.moves_to_go,
            depth: unified.depth,
            nodes: unified.nodes,
            time_parameters: unified.time_parameters,
        }
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

    #[test]
    fn test_conversion_from_basic() {
        let basic = super::super::search_basic::SearchLimits {
            depth: 8,
            time: Some(Duration::from_millis(500)),
            nodes: Some(10000),
            stop_flag: None,
            info_callback: None,
        };

        let unified: SearchLimits = basic.into();
        assert_eq!(unified.depth, Some(8));
        assert_eq!(unified.node_limit(), Some(10000));
        assert_eq!(unified.time_limit(), Some(Duration::from_millis(500)));
    }

    #[test]
    fn test_conversion_roundtrip() {
        let original_basic = super::super::search_basic::SearchLimits {
            depth: 6,
            time: Some(Duration::from_millis(1500)),
            nodes: Some(20000),
            stop_flag: None,
            info_callback: None,
        };

        let unified: SearchLimits = original_basic.clone().into();
        let back_to_basic: super::super::search_basic::SearchLimits = unified.into();

        assert_eq!(back_to_basic.depth, original_basic.depth);
        assert_eq!(back_to_basic.time, original_basic.time);
        assert_eq!(back_to_basic.nodes, original_basic.nodes);
    }

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
}

// Manual Clone implementation for SearchLimits (info_callback is not cloneable)
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
        }
    }
}

// Manual Debug implementation for SearchLimits
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
