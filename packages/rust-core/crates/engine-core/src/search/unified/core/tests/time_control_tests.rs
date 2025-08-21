//! Tests for time control and event polling

use crate::evaluation::evaluate::MaterialEvaluator;
use crate::search::unified::core::time_control;
use crate::search::unified::UnifiedSearcher;
use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};
use crate::Color;
use std::sync::Arc;

#[test]
fn test_get_event_poll_mask_values() {
    // Test that the polling mask function returns expected values
    let evaluator = MaterialEvaluator;
    let searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);

    // Without time manager, should return 0x7F (127) for responsiveness
    let mask = time_control::get_event_poll_mask(&searcher);
    assert_eq!(mask, 0x7F, "Without time manager should return 0x7F");
}

#[test]
fn test_event_poll_mask_byoyomi() {
    // Test that byoyomi time control gets more frequent polling

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);

    // Set up byoyomi time control
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,  // Already in byoyomi
            byoyomi_ms: 6000, // 6 seconds
            periods: 1,
        },
        ..Default::default()
    };

    // Create TimeManager for byoyomi
    let time_manager = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);
    searcher.time_manager = Some(Arc::new(time_manager));

    // Get polling mask
    let mask = time_control::get_event_poll_mask(&searcher);

    // Should be 0x1F (check every 32 nodes) for byoyomi
    assert_eq!(mask, 0x1F, "Byoyomi should use frequent polling (every 32 nodes)");
}

#[test]
fn test_event_poll_mask_various_time_controls() {
    // Test polling masks for different time controls

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);

    // Test Fischer with short time
    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 1000,
            black_ms: 1000,
            increment_ms: 0,
        },
        ..Default::default()
    };
    let time_manager = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);
    searcher.time_manager = Some(Arc::new(time_manager));
    let mask = time_control::get_event_poll_mask(&searcher);
    assert!(mask <= 0x7F, "Short time Fischer should use frequent polling");

    // Test FixedTime
    let limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 100 },
        ..Default::default()
    };
    let time_manager = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);
    searcher.time_manager = Some(Arc::new(time_manager));
    let mask = time_control::get_event_poll_mask(&searcher);
    assert!(mask <= 0x3F, "FixedTime 100ms should use very frequent polling");
}
