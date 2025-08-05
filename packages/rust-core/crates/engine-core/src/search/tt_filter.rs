//! Transposition table filtering strategies
//!
//! This module provides filtering functions to reduce TT overhead by
//! selectively storing and prefetching entries based on search characteristics.

use crate::search::tt::NodeType;

/// Simple optimization: Skip TT storage for very shallow nodes and quiescence
#[inline(always)]
pub fn should_skip_tt_store(depth: u8, in_quiescence: bool) -> bool {
    // Skip quiescence entries - they add overhead with minimal benefit
    if in_quiescence {
        return true;
    }

    // Skip very shallow entries (depth < 2)
    // These are unlikely to be reused and add overhead
    if depth < 2 {
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
