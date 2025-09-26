//! Transposition table filtering strategies
//!
//! This module provides filtering functions to reduce TT overhead by
//! selectively storing and prefetching entries based on search characteristics.

use crate::search::NodeType;

/// Simple optimization: Skip TT storage for very shallow nodes
/// PV nodes are always stored regardless of depth
#[inline(always)]
pub fn should_skip_tt_store(depth: u8, is_pv: bool) -> bool {
    // Always store PV nodes - they are critical for move ordering
    if is_pv {
        return false;
    }

    // Skip only depth 0 entries for non-PV nodes（d=1 も保存許可して再利用性を上げる）
    // These are unlikely to be reused and add overhead
    if depth < 1 {
        return true;
    }

    false
}

/// Simple optimization: Only prefetch at reasonable depths
#[inline(always)]
pub fn should_skip_prefetch(depth: u8, move_index: usize) -> bool {
    // Skip prefetch at shallow depths (not worth the overhead)
    if depth < 3 {
        return true;
    }

    // Skip prefetch for late moves at deep depths
    if depth > 6 && move_index > 2 {
        return true;
    }

    false
}

/// Boost depth for important nodes to keep them in TT longer
#[inline(always)]
pub fn boost_tt_depth(base_depth: u8, node_type: NodeType) -> u8 {
    // Small boost for exact nodes (they're more valuable)
    if node_type == NodeType::Exact {
        return base_depth.saturating_add(1);
    }

    base_depth
}

/// Additional boost for PV nodes
#[inline(always)]
pub fn boost_pv_depth(base_depth: u8, is_pv: bool) -> u8 {
    // PV nodes get additional depth boost to ensure they stay in TT
    if is_pv {
        return base_depth.saturating_add(2);
    }
    base_depth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tt_store_keeps_pv_even_at_shallow_depth() {
        // PV nodes should always be stored, regardless of depth
        assert!(!should_skip_tt_store(0, true));
        assert!(!should_skip_tt_store(1, true));

        // Non-PV very shallow nodes should be skipped
        assert!(should_skip_tt_store(0, false));
        // depth=1 is now stored to improve early-iteration TT reuse
        assert!(!should_skip_tt_store(1, false));

        // Non-PV deeper nodes should be stored
        assert!(!should_skip_tt_store(2, false));
        assert!(!should_skip_tt_store(3, false));
    }

    #[test]
    fn test_depth_boosting() {
        // Test TT depth boost for exact nodes
        assert_eq!(boost_tt_depth(5, NodeType::Exact), 6);
        assert_eq!(boost_tt_depth(5, NodeType::LowerBound), 5);
        assert_eq!(boost_tt_depth(5, NodeType::UpperBound), 5);

        // Test PV depth boost
        assert_eq!(boost_pv_depth(5, true), 7);
        assert_eq!(boost_pv_depth(5, false), 5);

        // Test saturation
        assert_eq!(boost_tt_depth(255, NodeType::Exact), 255);
        assert_eq!(boost_pv_depth(254, true), 255);
    }
}
