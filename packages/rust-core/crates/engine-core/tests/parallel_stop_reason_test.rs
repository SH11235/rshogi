use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::types::TerminationReason;
use engine_core::search::SearchLimits;
use engine_core::shogi::Position;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn user_stop_sets_stop_info() {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let limits = SearchLimits::builder().depth(6).stop_flag(stop_flag.clone()).build();

    let handle = thread::spawn(move || {
        let mut engine = Engine::new(EngineType::Material);
        engine.set_threads(2);
        let mut pos = Position::startpos();
        engine.search(&mut pos, limits)
    });

    thread::sleep(Duration::from_millis(10));
    stop_flag.store(true, Ordering::Release);

    let result = handle.join().expect("search thread");
    let stop_info = result.stop_info.expect("stop info present");
    assert_eq!(stop_info.reason, TerminationReason::UserStop);
}

#[test]
fn time_limit_preserves_reason() {
    let mut engine = Engine::new(EngineType::Material);
    engine.set_threads(2);
    let mut pos = Position::startpos();

    let limits = SearchLimits::builder().fixed_time_ms(100).build();

    let result = engine.search(&mut pos, limits);
    let stop_info = result.stop_info.expect("stop info present");
    assert_eq!(stop_info.reason, TerminationReason::TimeLimit);
}
