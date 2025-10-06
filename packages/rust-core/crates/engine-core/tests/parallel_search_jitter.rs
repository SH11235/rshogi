use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{
        parallel::{ParallelSearcher, StopController},
        SearchLimitsBuilder, TranspositionTable,
    },
    shogi::Position,
};
use std::sync::Arc;

fn run_helper_share(threads: usize) -> f64 {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));
    let stop = Arc::new(StopController::new());
    let mut searcher = ParallelSearcher::<MaterialEvaluator>::new(
        Arc::clone(&evaluator),
        Arc::clone(&tt),
        threads,
        Arc::clone(&stop),
    );

    let mut position = Position::startpos();
    let limits = SearchLimitsBuilder::default().fixed_nodes(2_048).depth(4).build();
    let result = searcher.search(&mut position, limits);
    result.stats.duplication_percentage.unwrap_or(0.0)
}

#[test]
fn helper_share_increases_when_jitter_enabled() {
    let original = std::env::var("SHOGI_TEST_FORCE_JITTER").ok();

    std::env::set_var("SHOGI_TEST_FORCE_JITTER", "0");
    let share_off = run_helper_share(3);

    std::env::set_var("SHOGI_TEST_FORCE_JITTER", "1");
    let share_on = run_helper_share(3);

    match original {
        Some(val) => std::env::set_var("SHOGI_TEST_FORCE_JITTER", val),
        None => std::env::remove_var("SHOGI_TEST_FORCE_JITTER"),
    }

    assert!(
        (share_on - share_off).abs() >= 0.1,
        "expected jitter to materially change helper share, got on={share_on:.2} off={share_off:.2}"
    );
    assert!(share_on > 0.0);
}
