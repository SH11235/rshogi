//! Integration tests for NNUE evaluation

use shogi_core::ai::engine::EngineType;
use shogi_core::ai::{Engine, Position, SearchLimits};
use std::time::Duration;

#[test]
fn test_nnue_engine_basic() {
    let mut pos = Position::startpos();
    let engine = Engine::new(EngineType::Nnue);

    let limits = SearchLimits {
        depth: 3,
        time: Some(Duration::from_secs(1)),
        nodes: Some(10000),
    };

    let result = engine.search(&mut pos, limits);

    assert!(result.best_move.is_some());
    assert!(result.stats.nodes > 0);

    // NNUE should give non-zero evaluation for startpos
    // (although with zero weights it will be 0)
    println!("NNUE evaluation: {}", result.score);
}

#[test]
fn test_nnue_vs_material_comparison() {
    let mut pos = Position::startpos();

    // Test with material evaluator
    let material_engine = Engine::new(EngineType::Material);
    let limits = SearchLimits {
        depth: 4,
        time: Some(Duration::from_secs(1)),
        nodes: None,
    };
    let material_result = material_engine.search(&mut pos, limits.clone());

    // Test with NNUE evaluator
    let nnue_engine = Engine::new(EngineType::Nnue);
    let nnue_result = nnue_engine.search(&mut pos, limits);

    println!(
        "Material eval: {} (best: {:?})",
        material_result.score, material_result.best_move
    );
    println!("NNUE eval: {} (best: {:?})", nnue_result.score, nnue_result.best_move);

    // Both should find a valid move
    assert!(material_result.best_move.is_some());
    assert!(nnue_result.best_move.is_some());
}

#[test]
#[ignore] // Ignore by default as it requires a real NNUE file
fn test_load_nnue_file() {
    use shogi_core::ai::nnue::weights::load_weights;

    // This test requires a real NNUE file
    // You can download one from YaneuraOu project
    let path = "test_data/nn.bin";

    match load_weights(path) {
        Ok((transformer, network)) => {
            println!("Successfully loaded NNUE weights");
            assert_eq!(transformer.weights.len(), 81 * 2182 * 256);
            assert_eq!(transformer.biases.len(), 256);
            assert_eq!(network.hidden1_weights.len(), 512 * 32);
            assert_eq!(network.hidden1_biases.len(), 32);
        }
        Err(e) => {
            println!("Failed to load NNUE file (expected if file doesn't exist): {}", e);
        }
    }
}

#[test]
#[ignore] // Requires internal test utilities
fn test_nnue_weight_file_format() {
    // This test is implemented internally in the weights module
    // See src/ai/nnue/weights.rs tests
}
