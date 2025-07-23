//! NNUE performance benchmark

use std::time::{Duration, Instant};

use engine_core::{
    benchmark::{benchmark_evaluation, run_benchmark},
    engine::controller::{Engine, EngineType},
    search::search_basic::SearchLimits,
    Position,
};

fn main() {
    println!("=== NNUE Performance Benchmark ===\n");

    // Run comprehensive evaluation benchmark
    println!("1. Direct Evaluation Function Comparison");
    println!("========================================");
    let eval_comparison = benchmark_evaluation();

    println!("\nMaterial Evaluator:");
    println!("  - Evaluations/sec: {}", eval_comparison.material.evals_per_sec);
    println!("  - Avg time: {} ns", eval_comparison.material.avg_eval_time_ns);

    println!("\nNNUE Evaluator:");
    println!("  - Evaluations/sec: {}", eval_comparison.nnue.evals_per_sec);
    println!("  - Avg time: {} ns", eval_comparison.nnue.avg_eval_time_ns);

    println!("\nPerformance Comparison:");
    println!(
        "  - NNUE is {:.1}x slower than Material evaluator",
        eval_comparison.nnue_slowdown_factor
    );
    println!(
        "  - NNUE overhead: {:.1}%",
        (eval_comparison.nnue_slowdown_factor - 1.0) * 100.0
    );

    // Test positions for search benchmark
    println!("\n\n2. Search Performance Comparison");
    println!("=================================");
    let positions = vec![
        Position::startpos(),
        // Add more test positions here if needed
    ];

    // Compare Material vs NNUE evaluation in search
    for (i, pos) in positions.iter().enumerate() {
        println!("\nPosition {}:", i + 1);

        // Material evaluator benchmark
        let material_engine = Engine::new(EngineType::Material);
        let material_result = benchmark_engine(&material_engine, pos.clone(), "Material");

        // NNUE evaluator benchmark
        let nnue_engine = Engine::new(EngineType::Nnue);
        let nnue_result = benchmark_engine(&nnue_engine, pos.clone(), "NNUE");

        // Compare results
        println!("\nSearch Comparison:");
        println!("  Material NPS: {:.0}", material_result.0);
        println!("  NNUE NPS: {:.0}", nnue_result.0);
        println!("  NPS ratio: {:.2}x", material_result.0 / nnue_result.0);
        println!(
            "  NNUE search overhead: {:.1}%",
            (1.0 - nnue_result.0 / material_result.0) * 100.0
        );

        println!("\nEvaluation Quality:");
        println!("  Material score: {}", material_result.1);
        println!("  NNUE score: {}", nnue_result.1);
        println!("  Score difference: {}", (nnue_result.1 - material_result.1).abs());
    }

    // Run general benchmark
    println!("\n\n3. General Performance Metrics");
    println!("==============================");
    let general_result = run_benchmark();
    println!("Move Generation: {} moves/sec", general_result.movegen_speed);
    println!("Search NPS (Material): {} nodes/sec", general_result.nps);
}

fn benchmark_engine(engine: &Engine, mut pos: Position, name: &str) -> (f64, i32) {
    println!("\n  {name} Engine:");

    let limits = SearchLimits {
        depth: 8,
        time: Some(Duration::from_secs(5)), // Longer time for more accurate measurement
        nodes: None,
        stop_flag: None,
    };

    let start = Instant::now();
    let result = engine.search(&mut pos, limits);
    let elapsed = start.elapsed();

    let nps = result.stats.nodes as f64 / elapsed.as_secs_f64();

    println!("    Nodes: {}", result.stats.nodes);
    println!("    Time: {elapsed:?}");
    println!("    NPS: {nps:.0}");
    println!("    Best move: {:?}", result.best_move);
    println!("    Score: {}", result.score);

    (nps, result.score)
}

#[test]
fn test_nnue_performance() {
    // Simple performance regression test
    let pos = Position::startpos();
    let engine = Engine::new(EngineType::Nnue);

    let limits = SearchLimits {
        depth: 4,
        time: Some(Duration::from_millis(100)),
        nodes: None,
    };

    let start = Instant::now();
    let result = engine.search(&mut pos.clone(), limits);
    let elapsed = start.elapsed();

    // Should complete within reasonable time
    assert!(elapsed < Duration::from_secs(1));
    assert!(result.stats.nodes > 100);
}
