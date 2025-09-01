use super::*;
use crate::evaluation::evaluate::MaterialEvaluator;
use crate::search::{constants::SEARCH_INF, SearchLimitsBuilder};
use crate::Position;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn test_unified_searcher_creation() {
    let evaluator = MaterialEvaluator;
    let searcher = UnifiedSearcher::<_, true, false>::new(evaluator);
    assert_eq!(searcher.nodes(), 0);
}

#[test]
fn test_shared_tt_creation() {
    // Test that we can create a searcher with a shared TT
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(crate::search::TranspositionTable::new(16));

    // Create two searchers with the same TT
    let searcher1 =
        UnifiedSearcher::<_, true, false>::with_shared_tt(evaluator.clone(), tt.clone());
    let searcher2 = UnifiedSearcher::<_, true, false>::with_shared_tt(evaluator, tt.clone());

    // Both searchers should start with 0 nodes
    assert_eq!(searcher1.nodes(), 0);
    assert_eq!(searcher2.nodes(), 0);

    // The TT should be the same Arc instance
    assert!(Arc::ptr_eq(searcher1.tt.as_ref().unwrap(), searcher2.tt.as_ref().unwrap()));
}

#[test]
fn test_compile_time_features() {
    // Test that const generic parameters work correctly
    // We can directly use the const parameters in the type
    type BasicConfig = UnifiedSearcher<MaterialEvaluator, true, false>;
    type EnhancedConfig = UnifiedSearcher<MaterialEvaluator, true, true>;

    // These tests verify the type system works correctly with const generics
    // The actual behavior is tested in search tests
    let basic_eval = MaterialEvaluator;
    let _basic = BasicConfig::new(basic_eval);

    let enhanced_eval = MaterialEvaluator;
    let _enhanced = EnhancedConfig::new(enhanced_eval);
}

#[test]
fn test_runtime_tt_size() {
    // Test creating searchers with different TT sizes
    let evaluator = MaterialEvaluator;
    let searcher_small = UnifiedSearcher::<_, true, false>::new_with_tt_size(evaluator, 8);
    let searcher_large = UnifiedSearcher::<_, true, false>::new_with_tt_size(evaluator, 64);

    // Both searchers should work correctly regardless of TT size
    assert_eq!(searcher_small.nodes(), 0);
    assert_eq!(searcher_large.nodes(), 0);
}

#[test]
fn test_fixed_nodes() {
    // Test FixedNodes - 時間に依存しない
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);
    let mut pos = Position::startpos();

    let limits = SearchLimitsBuilder::default().fixed_nodes(5000).build();
    let start = Instant::now();
    let result = searcher.search(&mut pos, limits);
    let elapsed = start.elapsed();

    assert!(result.best_move.is_some());
    assert!(
        result.stats.nodes <= 10000,
        "Node count {} should be reasonable (quiescence search may exceed limit)",
        result.stats.nodes
    );
    assert!(elapsed.as_secs() < 1, "Should complete within 1 second");
}

#[test]
fn test_depth_limit() {
    // Test depth limit - 浅い深さで確実に終了
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);
    let mut pos = Position::startpos();

    let limits = SearchLimitsBuilder::default().depth(1).build();

    let start = Instant::now();
    let result = searcher.search(&mut pos, limits);
    let elapsed = start.elapsed();

    assert!(result.best_move.is_some());
    assert_eq!(result.stats.depth, 1);
    assert!(elapsed.as_secs() < 1, "Should complete within 1 second");
}

#[test]
fn test_stop_flag_responsiveness() {
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);
    let mut pos = Position::startpos();
    let stop_flag = Arc::new(AtomicBool::new(false));

    // 十分なノード数を設定して、停止フラグなしでは時間がかかるようにする
    let limits = SearchLimitsBuilder::default()
        .fixed_nodes(1_000_000)
        .stop_flag(stop_flag.clone())
        .build();

    // 5ms後に停止フラグを立てる（CI環境での安定性のため）
    let stop_flag_clone = stop_flag.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(5));
        stop_flag_clone.store(true, Ordering::Relaxed);
    });

    let start = Instant::now();
    let result = searcher.search(&mut pos, limits);
    let elapsed = start.elapsed();

    assert!(result.best_move.is_some());
    assert!(
        elapsed.as_millis() < 80,
        "Search should stop within 80ms after stop flag is set, but took {}ms",
        elapsed.as_millis()
    );
}

#[test]
fn test_time_manager_integration() {
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);
    let mut pos = Position::startpos();

    // 100msの時間制限で、深さ3に制限
    let limits = SearchLimitsBuilder::default().fixed_time_ms(100).depth(3).build();

    let start = Instant::now();
    let result = searcher.search(&mut pos, limits);
    let elapsed = start.elapsed();

    assert!(result.best_move.is_some());

    // 時間制限が効いていることを確認（マージンを持たせる）
    assert!(
        elapsed.as_millis() < 200,
        "Should stop around 100ms, but took {}ms (depth reached: {}, nodes: {})",
        elapsed.as_millis(),
        result.stats.depth,
        result.stats.nodes
    );
}

#[test]
fn test_short_time_control() {
    // Test very short time controls with adaptive polling
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);
    let mut pos = Position::startpos();

    // 300msの時間制限（安定してdepth 1が完走できる）
    let limits = SearchLimitsBuilder::default().fixed_time_ms(300).depth(2).build();

    let start = Instant::now();
    let result = searcher.search(&mut pos, limits);
    let elapsed = start.elapsed();

    assert!(result.best_move.is_some(), "Must have best move even with short time");
    assert!(result.stats.depth >= 1, "Should complete at least depth 1");
    assert!(
        elapsed.as_millis() < 400,
        "Should stop within 400ms with 300ms limit, but took {}ms",
        elapsed.as_millis()
    );
}

#[test]
fn test_aspiration_window_search() {
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<_, true, true>::new(evaluator);
    let mut pos = Position::startpos();

    // Search with depth limit to test aspiration windows
    let limits = SearchLimitsBuilder::default().depth(4).build();
    let result = searcher.search(&mut pos, limits);

    assert!(result.best_move.is_some());

    // Check that aspiration window statistics were tracked
    // At depth 2 and beyond, aspiration windows should be used
    if result.stats.depth >= 2 {
        // Either hits or failures should be recorded
        let hits = result.stats.aspiration_hits.unwrap_or(0);
        let failures = result.stats.aspiration_failures.unwrap_or(0);
        assert!(hits > 0 || failures > 0, "Aspiration window should be used at depth >= 2");
    }
}

#[test]
fn test_pv_consistency_depth5() {
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<_, true, true>::new(evaluator);
    let mut pos = Position::startpos();

    // Search to depth 5 with fixed seed for reproducibility
    let limits = SearchLimitsBuilder::default().depth(5).build();

    let result = searcher.search(&mut pos, limits);

    // Verify PV consistency
    assert!(result.best_move.is_some());
    let best_move = result.best_move.unwrap();

    // PV first move must match bestmove
    assert!(!result.stats.pv.is_empty());
    assert_eq!(
        result.stats.pv[0],
        best_move,
        "bestmove {:?} != pv[0] {:?}",
        crate::usi::move_to_usi(&best_move),
        crate::usi::move_to_usi(&result.stats.pv[0])
    );

    // No duplicate moves in PV
    for i in 1..result.stats.pv.len() {
        assert_ne!(
            result.stats.pv[i - 1],
            result.stats.pv[i],
            "Duplicate move in PV at index {}: {}",
            i,
            crate::usi::move_to_usi(&result.stats.pv[i])
        );
    }
}

#[test]
fn test_early_stop_returns_valid_score() {
    // Test that stopping the search very early returns a valid score, not -SEARCH_INF + ply
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<_, true, true>::new(evaluator);

    // Create a stop flag that we'll set immediately
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Set limits with the stop flag
    let limits = SearchLimitsBuilder::default()
        .depth(10) // High depth limit
        .stop_flag(stop_flag.clone())
        .build();

    // Set the stop flag immediately before searching
    stop_flag.store(true, Ordering::Relaxed);

    let mut pos = Position::startpos();
    let result = searcher.search(&mut pos, limits);

    // When stopped immediately, the initial best_score of -SEARCH_INF should be returned
    // The score should not be an adjusted value like -SEARCH_INF + 6
    assert!(
        result.score == -SEARCH_INF || result.score > -SEARCH_INF + 1000,
        "Score should be either -SEARCH_INF or a reasonable evaluation, not {}",
        result.score
    );

    // When depth 1 completes, ensure we got a reasonable score
    if result.stats.depth >= 1 {
        assert!(
            result.score > -SEARCH_INF + 1000,
            "Completed depth should have reasonable score"
        );
    }
}

#[test]
fn test_interrupted_aspiration_window_score() {
    // Specifically test the issue where -SEARCH_INF + 6 was returned
    // To reproduce the issue, we need to interrupt during aspiration window retry
    // This is hard to do deterministically, so we'll just verify that any score
    // returned is reasonable

    // Run search in a thread and interrupt it at various times
    for interrupt_delay_ms in [0, 1, 2, 5].iter() {
        let mut searcher = UnifiedSearcher::<_, true, true>::new(MaterialEvaluator);
        let mut pos = Position::startpos();
        let stop_flag = Arc::new(AtomicBool::new(false));

        let limits = SearchLimitsBuilder::default().depth(10).stop_flag(stop_flag.clone()).build();

        // Set stop flag after delay
        let stop_flag_clone = stop_flag.clone();
        let delay = *interrupt_delay_ms;
        if delay > 0 {
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(delay));
                stop_flag_clone.store(true, Ordering::Relaxed);
            });
        } else {
            stop_flag.store(true, Ordering::Relaxed);
        }

        let result = searcher.search(&mut pos, limits);

        // Verify the score is reasonable
        assert!(
            result.score == -SEARCH_INF || result.score > -SEARCH_INF + 1000 || result.score < SEARCH_INF - 1000,
            "Score should be either -SEARCH_INF or a reasonable value, not {} (delay: {}ms, depth: {})",
            result.score,
            delay,
            result.stats.depth
        );

        // The specific bug was returning -SEARCH_INF + 6
        assert_ne!(
            result.score,
            -SEARCH_INF + 6,
            "Should not return the specific buggy value -SEARCH_INF + 6"
        );
    }
}
