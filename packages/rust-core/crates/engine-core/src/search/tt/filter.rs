//! Transposition table filtering strategies
//!
//! This module provides filtering functions to reduce TT overhead by
//! selectively storing and prefetching entries based on search characteristics.

use crate::search::NodeType;

/// Dynamic TT store filter that adapts to table occupancy (hashfull)
///
/// Skips storing entries to reduce TT overhead when:
/// - depth=0 (qsearch): High volatility, low reuse rate
/// - hashfull>=90% AND depth<=2 AND non-Exact bounds: Preserve TT space
///
/// Always stores:
/// - PV nodes (critical for move ordering)
/// - Exact nodes (highest value)
/// - depth>=3 (sufficient search depth for reuse)
#[inline(always)]
pub fn should_skip_tt_store_dyn(
    depth: u8,
    is_pv: bool,
    node_type: NodeType,
    hashfull_permille: u16, // permille (0..=1000)
) -> bool {
    if is_pv {
        return false;
    }
    // Skip qsearch (depth=0) - high volatility, low reuse rate
    if depth == 0 {
        return true;
    }
    // When table is very full, skip storing shallow non-Exact bounds for non-PV nodes
    if hashfull_permille >= 900 && depth <= 2 && node_type != NodeType::Exact {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pv_nodes_always_stored() {
        // PV nodes should never be skipped, regardless of depth or hashfull
        assert!(!should_skip_tt_store_dyn(0, true, NodeType::Exact, 0));
        assert!(!should_skip_tt_store_dyn(0, true, NodeType::LowerBound, 0));
        assert!(!should_skip_tt_store_dyn(1, true, NodeType::UpperBound, 0));
        assert!(!should_skip_tt_store_dyn(5, true, NodeType::LowerBound, 1000));
        assert!(!should_skip_tt_store_dyn(0, true, NodeType::Exact, 1000));
    }

    #[test]
    fn test_qsearch_depth_skipped() {
        // depth=0 (qsearch) should be skipped for non-PV nodes
        assert!(should_skip_tt_store_dyn(0, false, NodeType::Exact, 0));
        assert!(should_skip_tt_store_dyn(0, false, NodeType::LowerBound, 0));
        assert!(should_skip_tt_store_dyn(0, false, NodeType::UpperBound, 0));

        // But depth >= 1 should not be skipped at low hashfull
        assert!(!should_skip_tt_store_dyn(1, false, NodeType::Exact, 0));
        assert!(!should_skip_tt_store_dyn(1, false, NodeType::LowerBound, 0));
    }

    #[test]
    fn test_hashfull_filter_shallow_bounds() {
        // At high hashfull (>=90%), shallow non-Exact bounds should be skipped
        assert!(should_skip_tt_store_dyn(1, false, NodeType::LowerBound, 900));
        assert!(should_skip_tt_store_dyn(2, false, NodeType::UpperBound, 900));
        assert!(should_skip_tt_store_dyn(1, false, NodeType::LowerBound, 1000));

        // But Exact nodes should NOT be skipped, even at high hashfull
        assert!(!should_skip_tt_store_dyn(1, false, NodeType::Exact, 900));
        assert!(!should_skip_tt_store_dyn(2, false, NodeType::Exact, 1000));
    }

    #[test]
    fn test_hashfull_filter_not_applied_at_low_occupancy() {
        // At low hashfull (<90%), even shallow bounds should be stored
        assert!(!should_skip_tt_store_dyn(1, false, NodeType::LowerBound, 0));
        assert!(!should_skip_tt_store_dyn(2, false, NodeType::UpperBound, 500));
        assert!(!should_skip_tt_store_dyn(1, false, NodeType::LowerBound, 899));
    }

    #[test]
    fn test_depth_3_always_stored() {
        // depth >= 3 should always be stored (sufficient search depth)
        assert!(!should_skip_tt_store_dyn(3, false, NodeType::LowerBound, 1000));
        assert!(!should_skip_tt_store_dyn(4, false, NodeType::UpperBound, 1000));
        assert!(!should_skip_tt_store_dyn(10, false, NodeType::LowerBound, 1000));
    }

    #[test]
    fn test_exact_nodes_protected() {
        // Exact nodes should be stored at all depths (except qsearch) and hashfull levels
        assert!(!should_skip_tt_store_dyn(1, false, NodeType::Exact, 0));
        assert!(!should_skip_tt_store_dyn(1, false, NodeType::Exact, 500));
        assert!(!should_skip_tt_store_dyn(1, false, NodeType::Exact, 900));
        assert!(!should_skip_tt_store_dyn(2, false, NodeType::Exact, 1000));
        assert!(!should_skip_tt_store_dyn(5, false, NodeType::Exact, 1000));
    }

    #[test]
    fn test_boundary_conditions() {
        // Test exact boundary values

        // Hashfull boundary: 899 vs 900
        assert!(!should_skip_tt_store_dyn(2, false, NodeType::LowerBound, 899));
        assert!(should_skip_tt_store_dyn(2, false, NodeType::LowerBound, 900));

        // Depth boundary: 2 vs 3
        assert!(should_skip_tt_store_dyn(2, false, NodeType::LowerBound, 900));
        assert!(!should_skip_tt_store_dyn(3, false, NodeType::LowerBound, 900));

        // Qsearch boundary: 0 vs 1
        assert!(should_skip_tt_store_dyn(0, false, NodeType::Exact, 0));
        assert!(!should_skip_tt_store_dyn(1, false, NodeType::Exact, 0));
    }

    #[test]
    fn test_comprehensive_matrix() {
        // Comprehensive test matrix covering all combinations

        // Low hashfull (0-500): Only qsearch skipped
        for depth in 0..10 {
            for hashfull in [0, 250, 500] {
                for node_type in [NodeType::Exact, NodeType::LowerBound, NodeType::UpperBound] {
                    let skip = should_skip_tt_store_dyn(depth, false, node_type, hashfull);
                    if depth == 0 {
                        assert!(skip, "depth={depth} hashfull={hashfull} node_type={node_type:?}");
                    } else {
                        assert!(!skip, "depth={depth} hashfull={hashfull} node_type={node_type:?}");
                    }
                }
            }
        }

        // High hashfull (900+): Qsearch + shallow bounds skipped
        for depth in 0..10 {
            for hashfull in [900, 950, 1000] {
                // Exact nodes
                let skip_exact = should_skip_tt_store_dyn(depth, false, NodeType::Exact, hashfull);
                if depth == 0 {
                    assert!(skip_exact, "depth={depth} hashfull={hashfull} Exact");
                } else {
                    assert!(!skip_exact, "depth={depth} hashfull={hashfull} Exact");
                }

                // Bound nodes
                for node_type in [NodeType::LowerBound, NodeType::UpperBound] {
                    let skip = should_skip_tt_store_dyn(depth, false, node_type, hashfull);
                    if depth == 0 || (depth <= 2) {
                        assert!(skip, "depth={depth} hashfull={hashfull} node_type={node_type:?}");
                    } else {
                        assert!(!skip, "depth={depth} hashfull={hashfull} node_type={node_type:?}");
                    }
                }
            }
        }
    }
}
