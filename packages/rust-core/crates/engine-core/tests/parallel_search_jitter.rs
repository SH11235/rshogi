use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{
        parallel::{ParallelSearcher, StopController},
        SearchLimitsBuilder, TranspositionTable,
    },
    shogi::Position,
};
use std::sync::Arc;

#[test]
fn helper_share_increases_when_jitter_enabled() {
    let share_off = run_helper_share_with(3, false);
    let share_on = run_helper_share_with(3, true);

    assert!(
        (share_on - share_off).abs() >= 0.1,
        "expected jitter to materially change helper share, got on={share_on:.2} off={share_off:.2}"
    );
    assert!(share_on > 0.0);
}

fn run_helper_share_with(threads: usize, jitter: bool) -> f64 {
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
    let limits = SearchLimitsBuilder::default()
        .fixed_nodes(8_192)
        .depth(4)
        .jitter_override(jitter)
        .build();
    let result = searcher.search(&mut position, limits);
    result.stats.helper_share_pct.unwrap_or(0.0)
}
