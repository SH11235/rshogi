use engine_core::search::policy as search_policy;
use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{
        parallel::{ParallelSearcher, StopController},
        SearchLimitsBuilder, TranspositionTable,
    },
    shogi::Position,
    time_management::{
        detect_game_phase_for_time, TimeControl as TMTimeControl, TimeLimits, TimeManager,
    },
    Color,
};
use std::sync::Arc;

#[test]
fn helper_share_positive_and_stable_with_or_without_jitter() {
    let share_off = run_helper_share_with(3, false);
    let share_on = run_helper_share_with(3, true);

    // 純粋 LazySMP では Queue を使わず、jitter は多様化のための軽い擾乱に留まる。
    // helper_share は主にスレッド数/探索量で決まり、jitter の有無で大きくは変わらない想定。
    // したがって「大幅に変化しない」ことと「両ケースでヘルパー寄与が正」だけを検証する。
    assert!(share_off > 0.0, "helper_share without jitter should be positive");
    assert!(share_on > 0.0, "helper_share with jitter should be positive");

    let diff = (share_on - share_off).abs();
    assert!(
        diff <= 25.0,
        "helper_share should be broadly stable; diff={diff:.2} on={share_on:.2} off={share_off:.2}"
    );
}

fn run_helper_share_with(threads: usize, jitter: bool) -> f64 {
    // 全件待機に切り替えて helper のノード計上漏れを防ぐ（テスト専用）
    search_policy::set_bench_allrun(true);
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
    // FixedNodes を厳密に効かせるため、TimeManager を明示的に付与する。
    // 注意: LazySMP では TimeManager が無い場合、FixedNodes は停止条件として扱われない。
    let tm_limits = TimeLimits {
        time_control: TMTimeControl::FixedNodes { nodes: 8_192 },
        ..Default::default()
    };
    let tm =
        TimeManager::new(&tm_limits, Color::Black, 0, detect_game_phase_for_time(&position, 0));

    let mut limits = SearchLimitsBuilder::default()
        .fixed_nodes(8_192)
        .depth(4)
        .jitter_override(jitter)
        .build();
    // テスト安定化のために TimeManager を同伴させる
    limits.time_manager = Some(Arc::new(tm));
    let result = searcher.search(&mut position, limits);
    // グローバルを元に戻す（他テストへの影響を避ける）
    search_policy::set_bench_allrun(false);
    result.stats.helper_share_pct.unwrap_or(0.0)
}
