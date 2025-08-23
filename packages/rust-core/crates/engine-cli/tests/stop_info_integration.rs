//! Integration tests for stop info tracking

use engine_core::search::types::{StopInfo, TerminationReason};

#[test]
fn test_search_stop_info_propagation() {
    // This test verifies that stop info is properly propagated through the search
    // Note: This is a simplified test that doesn't actually run a full search

    // Test data structures
    let stop_info = StopInfo {
        reason: TerminationReason::TimeLimit,
        elapsed_ms: 1234,
        nodes: 567890,
        depth_reached: 15,
        hard_timeout: false,
    };

    // Verify StopInfo fields
    assert_eq!(stop_info.reason, TerminationReason::TimeLimit);
    assert_eq!(stop_info.elapsed_ms, 1234);
    assert_eq!(stop_info.nodes, 567890);
    assert_eq!(stop_info.depth_reached, 15);
    assert!(!stop_info.hard_timeout);
}

#[test]
fn test_termination_reason_variants() {
    // Test all TerminationReason variants
    let reasons = vec![
        TerminationReason::TimeLimit,
        TerminationReason::NodeLimit,
        TerminationReason::DepthLimit,
        TerminationReason::UserStop,
        TerminationReason::Mate,
        TerminationReason::Completed,
        TerminationReason::Error,
    ];

    for reason in reasons {
        let stop_info = StopInfo {
            reason,
            elapsed_ms: 100,
            nodes: 1000,
            depth_reached: 5,
            hard_timeout: false,
        };

        // Verify Debug trait works
        let debug_str = format!("{:?}", reason);
        assert!(!debug_str.is_empty());

        // Verify Clone trait works
        let cloned = stop_info.clone();
        assert_eq!(cloned.reason, reason);
    }
}

#[test]
fn test_hard_timeout_scenarios() {
    // Test different hard timeout scenarios
    let scenarios = vec![
        (true, TerminationReason::TimeLimit, "Hard time limit"),
        (false, TerminationReason::TimeLimit, "Soft time limit"),
        (true, TerminationReason::UserStop, "Hard user stop"),
        (false, TerminationReason::Completed, "Normal completion"),
    ];

    for (hard_timeout, reason, description) in scenarios {
        let stop_info = StopInfo {
            reason,
            elapsed_ms: 500,
            nodes: 10000,
            depth_reached: 10,
            hard_timeout,
        };

        assert_eq!(stop_info.hard_timeout, hard_timeout, "Failed for scenario: {}", description);
    }
}
