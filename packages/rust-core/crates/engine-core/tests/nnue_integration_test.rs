//! Integration tests for NNUE evaluation

use engine_core::{
    engine::controller::{Engine, EngineType},
    evaluate::Evaluator,
    nnue::{weights::load_weights, NNUEEvaluatorWrapper},
    search::SearchLimits,
    Color, Piece, PieceType, Position, Square,
};

#[test]
#[ignore] // Requires large stack size due to NNUE initialization
fn test_nnue_engine_basic() {
    let mut pos = Position::startpos();
    let engine = Engine::new(EngineType::Nnue);

    let limits = SearchLimits::builder().depth(3).fixed_time_ms(1000).nodes(10000).build();

    let result = engine.search(&mut pos, limits);

    assert!(result.best_move.is_some());
    assert!(result.stats.nodes > 0);

    // NNUE should give non-zero evaluation for startpos
    // (although with zero weights it will be 0)
    println!("NNUE evaluation: {}", result.score);
}

#[test]
#[ignore] // Requires large stack size due to engine initialization
fn test_nnue_vs_material_comparison() {
    let mut pos = Position::startpos();

    // Test with material evaluator
    let material_engine = Engine::new(EngineType::Material);
    let limits = SearchLimits::builder().depth(4).fixed_time_ms(1000).build();
    let material_result = material_engine.search(&mut pos, limits);

    // Test with NNUE evaluator
    let nnue_engine = Engine::new(EngineType::Nnue);
    let limits2 = SearchLimits::builder().depth(4).fixed_time_ms(1000).build();
    let nnue_result = nnue_engine.search(&mut pos, limits2);

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
#[ignore] // Requires large stack size due to NNUE initialization
fn test_load_nnue_file() {
    use std::fs;
    use std::path::Path;

    // Decompress the mock NNUE file
    let compressed_path = "tests/data/mock_nn.bin.gz";
    let decompressed_path = "tests/data/nn.bin";

    // Check if compressed file exists
    if !Path::new(compressed_path).exists() {
        panic!(
            "Mock NNUE file not found at {compressed_path}. Run 'cargo run --bin create_mock_nnue' first."
        );
    }

    // Decompress the file
    let compressed_data = fs::read(compressed_path).expect("Failed to read compressed file");
    let mut decoder = flate2::read::GzDecoder::new(&compressed_data[..]);
    let mut decompressed_data = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decompressed_data).expect("Failed to decompress");

    // Write decompressed data
    fs::write(decompressed_path, &decompressed_data).expect("Failed to write decompressed file");

    // Test loading the file
    match load_weights(decompressed_path) {
        Ok((transformer, network)) => {
            println!("Successfully loaded NNUE weights");
            assert_eq!(transformer.weights.len(), 81 * 2182 * 256);
            assert_eq!(transformer.biases.len(), 256);
            assert_eq!(network.hidden1_weights.len(), 512 * 32);
            assert_eq!(network.hidden1_biases.len(), 32);

            // Verify that we have some non-zero weights (sparse initialization)
            let non_zero_ft_weights = transformer.weights.iter().filter(|&&w| w != 0).count();
            let non_zero_h1_weights = network.hidden1_weights.iter().filter(|&&w| w != 0).count();

            println!("Non-zero feature transformer weights: {non_zero_ft_weights}");
            println!("Non-zero hidden1 weights: {non_zero_h1_weights}");

            // Should have some non-zero weights but not all
            assert!(non_zero_ft_weights > 0);
            assert!(non_zero_ft_weights < transformer.weights.len() / 10); // Less than 10%
            assert!(non_zero_h1_weights > 0);
            assert!(non_zero_h1_weights < network.hidden1_weights.len() / 10);
        }
        Err(e) => {
            panic!("Failed to load NNUE file: {e}");
        }
    }

    // Clean up
    fs::remove_file(decompressed_path).ok();
}

#[test]
#[ignore] // Requires large stack size due to NNUE initialization
fn test_nnue_evaluation_with_mock_weights() {
    use std::fs;
    use std::path::Path;

    // Decompress the mock NNUE file
    let compressed_path = "tests/data/mock_nn.bin.gz";
    let decompressed_path = "tests/data/nn_eval_test.bin";

    if !Path::new(compressed_path).exists() {
        panic!("Mock NNUE file not found. Run 'cargo run --bin create_mock_nnue' first.");
    }

    // Decompress
    let compressed_data = fs::read(compressed_path).expect("Failed to read compressed file");
    let mut decoder = flate2::read::GzDecoder::new(&compressed_data[..]);
    let mut decompressed_data = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decompressed_data).expect("Failed to decompress");
    fs::write(decompressed_path, &decompressed_data).expect("Failed to write decompressed file");

    // Create evaluator with mock weights
    let wrapper =
        NNUEEvaluatorWrapper::new(decompressed_path).expect("Failed to create NNUE evaluator");

    // Test evaluation on different positions
    let positions = vec![Position::startpos(), {
        // Create a different position
        let mut pos = Position::empty();
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(Square::new(5, 7), Piece::new(PieceType::Gold, Color::Black));
        pos
    }];

    let mut evaluations = Vec::new();
    for pos in &positions {
        let eval = wrapper.evaluate(pos);
        evaluations.push(eval);
        println!("Position evaluation: {eval}");
    }

    // With sparse random weights, evaluations should not all be zero
    // (unlike zero weights which always give 0)
    let non_zero_evals = evaluations.iter().filter(|&&e| e != 0).count();
    assert!(non_zero_evals > 0, "At least some evaluations should be non-zero");

    // The evaluations should be in a reasonable range (not overflow)
    for eval in &evaluations {
        assert!(eval.abs() < 10000, "Evaluation {eval} is too large");
    }

    // Clean up
    fs::remove_file(decompressed_path).ok();
}
