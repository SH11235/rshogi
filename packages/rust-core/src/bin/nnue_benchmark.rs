//! NNUE performance benchmark

use shogi_core::ai::engine::EngineType;
use shogi_core::ai::{Engine, Position, SearchLimits};
use std::time::{Duration, Instant};

fn main() {
    println!("=== NNUE Performance Benchmark ===\n");

    // Test positions
    let positions = vec![
        Position::startpos(),
        // Add more test positions here if needed
    ];

    // Compare Material vs NNUE evaluation
    for (i, pos) in positions.iter().enumerate() {
        println!("Position {}:", i + 1);

        // Material evaluator benchmark
        let material_engine = Engine::new(EngineType::Material);
        let material_result = benchmark_engine(&material_engine, pos.clone(), "Material");

        // NNUE evaluator benchmark
        let nnue_engine = Engine::new(EngineType::Nnue);
        let nnue_result = benchmark_engine(&nnue_engine, pos.clone(), "NNUE");

        // Compare results
        println!("\nComparison:");
        println!("  Material NPS: {:.0}", material_result.0);
        println!("  NNUE NPS: {:.0}", nnue_result.0);
        println!("  NNUE overhead: {:.1}%", (1.0 - nnue_result.0 / material_result.0) * 100.0);
        println!();
    }
}

fn benchmark_engine(engine: &Engine, mut pos: Position, name: &str) -> (f64, i32) {
    println!("\n  {name} Engine:");

    let limits = SearchLimits {
        depth: 8,
        time: Some(Duration::from_secs(5)), // Longer time for more accurate measurement
        nodes: None,
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
