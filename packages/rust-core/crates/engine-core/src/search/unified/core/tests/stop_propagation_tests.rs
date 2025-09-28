//! Tests for stop propagation and time-based short-circuiting

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::evaluation::evaluate::MaterialEvaluator;
use crate::search::limits::SearchLimits;
use crate::search::unified::UnifiedSearcher;
use crate::shogi::Position;
use crate::time_management::{mock_set_time, GamePhase, TimeControl, TimeLimits, TimeManager};

/// Ensure that when hard limit is reached at alpha-beta entry, we short-circuit,
/// mark context.stop(), and propagate to external stop flag without lag.
#[test]
fn test_alpha_beta_hard_short_circuit_propagates_stop() {
    // Start mock time at 0ms
    mock_set_time(0);

    // Build a TimeManager with fixed time per move (will be clamped >= 50ms)
    let limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 50 },
        ..Default::default()
    };
    let tm = Arc::new(TimeManager::new_with_mock_time(
        &limits,
        crate::Color::Black,
        0,
        GamePhase::Opening,
    ));

    // Advance mock time beyond hard limit so alpha_beta() sees deadline immediately
    let hard = tm.hard_limit_ms();
    mock_set_time(hard.saturating_add(1));

    // Prepare searcher and attach external TimeManager
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(evaluator);
    searcher.set_time_manager_external(tm.clone());

    // Wire external stop flag via SearchLimits into the context so propagation can be observed
    let ext_stop = Arc::new(AtomicBool::new(false));
    let limits_for_context: SearchLimits =
        SearchLimits::builder().depth(2).stop_flag(ext_stop.clone()).build();
    searcher.context.set_limits(limits_for_context);

    let mut pos = Position::startpos();
    let alpha0 = -3456; // sentinel alpha to verify early return path
    let ret = super::super::alpha_beta(&mut searcher, &mut pos, 2, alpha0, 3456, 0);

    // Returned value should be the untouched alpha (early return path)
    assert_eq!(ret, alpha0, "alpha-beta should short-circuit and return initial alpha");

    // Context stop must be true with no lag
    assert!(
        searcher.context.should_stop(),
        "context.should_stop() must be true after short-circuit"
    );

    // External stop flag must be set as well for parallel propagation
    assert!(
        ext_stop.load(Ordering::Acquire),
        "external stop flag must be set by context.stop()"
    );
}

/// Ensure unified search attaches StopInfo when time-based stop occurs
#[test]
fn test_unified_search_attaches_stop_info_on_time_stop() {
    // Reset and set mock time baseline
    mock_set_time(0);

    // FixedTime with a small budget (min clamps apply inside TimeManager)
    let tl = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 50 },
        ..Default::default()
    };
    let tm =
        Arc::new(TimeManager::new_with_mock_time(&tl, crate::Color::Black, 0, GamePhase::Opening));

    // Advance time beyond hard to force time stop during search
    let hard = tm.hard_limit_ms();
    mock_set_time(hard.saturating_add(1));

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(evaluator);
    searcher.set_time_manager_external(tm.clone());

    let mut pos = Position::startpos();
    let limits = SearchLimits::builder().depth(4).build();
    let result = searcher.search(&mut pos, limits);

    // StopInfo should be present and indicate time-based termination
    let info = result.stop_info.as_ref().expect("StopInfo should be present on time stop");
    assert_eq!(info.reason.to_string(), "time_limit");
    // hard_timeout may be true or false depending on clamp; ensure consistency with elapsed/hard
    if info.hard_limit_ms > 0 {
        assert!(
            info.elapsed_ms >= info.hard_limit_ms || !info.hard_timeout,
            "hard_timeout flag must reflect elapsed vs hard"
        );
    }
}
