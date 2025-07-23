// This test verifies that the time management API is used correctly
// and that deprecated APIs are not accessible from external crates

use engine_core::search::GamePhase;
use engine_core::time_management::{SearchLimits, TimeControl, TimeManager, TimeState};
use engine_core::Color;

#[test]
fn test_update_after_move_api_works() {
    let limits = SearchLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 5000,
            byoyomi_ms: 1000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);

    // New API should work
    tm.update_after_move(1000, TimeState::Main { main_left_ms: 4000 });

    // Verify state
    let info = tm.get_time_info();
    assert!(info.elapsed_ms < 100); // Should be quick in test
}

#[test]
fn test_time_state_enum_is_accessible() {
    // Verify all TimeState variants are accessible
    let _main_state = TimeState::Main { main_left_ms: 1000 };
    let _byoyomi_state = TimeState::Byoyomi { main_left_ms: 0 };
    let _non_byoyomi_state = TimeState::NonByoyomi;
}

#[test]
fn test_byoyomi_transition_with_new_api() {
    let limits = SearchLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 1000,
            byoyomi_ms: 1000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    // Transition to byoyomi
    tm.update_after_move(1500, TimeState::Main { main_left_ms: 1000 });

    let info = tm.get_time_info();
    assert!(info.byoyomi_info.is_some());
    let byoyomi_info = info.byoyomi_info.unwrap();
    assert!(byoyomi_info.in_byoyomi);
}

// The following would fail to compile if uncommented, proving finish_move is not public:
// #[test]
// fn test_finish_move_not_accessible() {
//     let limits = SearchLimits {
//         time_control: TimeControl::Byoyomi {
//             main_time_ms: 5000,
//             byoyomi_ms: 1000,
//             periods: 3,
//         },
//         ..Default::default()
//     };
//     
//     let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);
//     
//     // This should NOT compile - finish_move is not public
//     tm.finish_move(1000, Some(4000));
// }