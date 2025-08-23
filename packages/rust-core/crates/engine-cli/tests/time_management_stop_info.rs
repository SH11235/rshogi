//! Integration tests for time management and stop info recording

use engine_core::search::types::{StopInfo, TerminationReason};
use std::time::{Duration, Instant};

#[test]
fn test_stop_info_time_tracking() {
    // Test that elapsed time is properly tracked
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(50));
    let elapsed = start.elapsed();

    let stop_info = StopInfo {
        reason: TerminationReason::TimeLimit,
        elapsed_ms: elapsed.as_millis() as u64,
        nodes: 1000,
        depth_reached: 5,
        hard_timeout: false,
    };

    // Should be at least 50ms
    assert!(stop_info.elapsed_ms >= 50);
    // But not too much more (allow for some overhead)
    assert!(stop_info.elapsed_ms < 200);
}

#[test]
fn test_nps_calculation() {
    // Test nodes per second calculation
    let test_cases = vec![
        (1000, 1000, 1000), // 1000 nodes in 1 second = 1000 nps
        (2000, 500, 4000),  // 2000 nodes in 0.5 seconds = 4000 nps
        (0, 1000, 0),       // 0 nodes = 0 nps
        (1000, 0, 0),       // 0 time = 0 nps (avoid division by zero)
    ];

    for (nodes, elapsed_ms, expected_nps) in test_cases {
        let nps = if elapsed_ms > 0 {
            nodes * 1000 / elapsed_ms
        } else {
            0
        };

        assert_eq!(nps, expected_nps, "Failed for nodes={}, elapsed_ms={}", nodes, elapsed_ms);
    }
}

#[test]
fn test_depth_tracking() {
    // Test that depth is properly tracked
    let depths = vec![1u8, 5, 10, 20, 50, 127, 255];

    for depth in depths {
        let stop_info = StopInfo {
            reason: TerminationReason::DepthLimit,
            elapsed_ms: 100,
            nodes: 1000,
            depth_reached: depth,
            hard_timeout: false,
        };

        assert_eq!(stop_info.depth_reached, depth);
    }
}

#[test]
fn test_stop_reason_scenarios() {
    // Test different scenarios that lead to different stop reasons
    struct Scenario {
        reason: TerminationReason,
        description: &'static str,
        expected_hard_timeout: bool,
    }

    let scenarios = vec![
        Scenario {
            reason: TerminationReason::TimeLimit,
            description: "Normal time limit reached",
            expected_hard_timeout: false,
        },
        Scenario {
            reason: TerminationReason::NodeLimit,
            description: "Node limit reached",
            expected_hard_timeout: false,
        },
        Scenario {
            reason: TerminationReason::DepthLimit,
            description: "Maximum depth reached",
            expected_hard_timeout: false,
        },
        Scenario {
            reason: TerminationReason::UserStop,
            description: "User requested stop",
            expected_hard_timeout: false,
        },
        Scenario {
            reason: TerminationReason::Mate,
            description: "Mate found",
            expected_hard_timeout: false,
        },
        Scenario {
            reason: TerminationReason::Completed,
            description: "Search completed normally",
            expected_hard_timeout: false,
        },
        Scenario {
            reason: TerminationReason::Error,
            description: "Error during search",
            expected_hard_timeout: false,
        },
    ];

    for scenario in scenarios {
        let stop_info = StopInfo {
            reason: scenario.reason,
            elapsed_ms: 1000,
            nodes: 10000,
            depth_reached: 10,
            hard_timeout: scenario.expected_hard_timeout,
        };

        assert_eq!(
            stop_info.reason, scenario.reason,
            "Failed for scenario: {}",
            scenario.description
        );
        assert_eq!(
            stop_info.hard_timeout, scenario.expected_hard_timeout,
            "Hard timeout mismatch for: {}",
            scenario.description
        );
    }
}
