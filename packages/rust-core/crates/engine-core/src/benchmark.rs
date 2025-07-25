//! Benchmark for AI performance testing

use crate::evaluate::{Evaluator, MaterialEvaluator};
use crate::movegen::MoveGen;
use crate::nnue::NNUEEvaluatorWrapper;
use crate::search::search_basic::Searcher;
use crate::search::SearchLimits;
use crate::shogi::MoveList;
use crate::shogi::{Color, Piece, PieceType, Position, Square};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Performance test results
#[derive(Debug)]
pub struct BenchmarkResult {
    /// Nodes per second
    pub nps: u64,
    /// Total nodes searched
    pub nodes: u64,
    /// Time elapsed
    pub elapsed: Duration,
    /// Move generation speed (moves/sec)
    pub movegen_speed: u64,
}

/// Evaluation benchmark results
#[derive(Debug)]
pub struct EvaluationBenchmarkResult {
    /// Evaluator name
    pub name: String,
    /// Evaluations per second
    pub evals_per_sec: u64,
    /// Average evaluation time (nanoseconds)
    pub avg_eval_time_ns: u64,
    /// Total evaluations
    pub total_evals: u64,
}

/// Comparison result between evaluators
#[derive(Debug)]
pub struct ComparisonResult {
    /// Material evaluator results
    pub material: EvaluationBenchmarkResult,
    /// NNUE evaluator results
    pub nnue: EvaluationBenchmarkResult,
    /// Performance ratio (NNUE slowdown factor)
    pub nnue_slowdown_factor: f64,
}

/// Run performance benchmark
pub fn run_benchmark() -> BenchmarkResult {
    println!("Running Shogi AI Benchmark...");

    // Test move generation speed
    let movegen_result = benchmark_movegen();
    println!("Move generation: {movegen_result} moves/sec");

    // Test search performance
    let search_result = benchmark_search();

    BenchmarkResult {
        nps: search_result.0,
        nodes: search_result.1,
        elapsed: search_result.2,
        movegen_speed: movegen_result,
    }
}

/// Benchmark move generation
fn benchmark_movegen() -> u64 {
    let pos = Position::startpos();
    let iterations = 100_000;

    let start = Instant::now();

    for _ in 0..iterations {
        let mut gen = MoveGen::new();
        let mut moves = MoveList::new();
        gen.generate_all(&pos, &mut moves);
        // Force evaluation to prevent optimization
        std::hint::black_box(moves.len());
    }

    let elapsed = start.elapsed();
    (iterations as f64 / elapsed.as_secs_f64()) as u64
}

/// Benchmark search performance
fn benchmark_search() -> (u64, u64, Duration) {
    let test_positions = vec![
        // Starting position
        Position::startpos(),
        // TODO: Add more test positions
    ];

    let mut total_nodes = 0u64;
    let mut total_time = Duration::ZERO;

    for (i, pos) in test_positions.iter().enumerate() {
        println!("Testing position {}...", i + 1);

        let limits = SearchLimits::builder().depth(8).fixed_time_ms(5000).build();

        let evaluator = Arc::new(MaterialEvaluator);
        let mut searcher = Searcher::new(limits, evaluator);
        let mut pos_clone = pos.clone();
        let result = searcher.search(&mut pos_clone);

        total_nodes += result.stats.nodes;
        total_time += result.stats.elapsed;

        println!(
            "  Best move: {:?}, Score: {}, Nodes: {}, Time: {:?}",
            result.best_move, result.score, result.stats.nodes, result.stats.elapsed
        );
    }

    let nps = (total_nodes as f64 / total_time.as_secs_f64()) as u64;

    (nps, total_nodes, total_time)
}

/// Benchmark evaluation function performance
pub fn benchmark_evaluation() -> ComparisonResult {
    println!("\nRunning Evaluation Function Benchmark...");

    // Create test positions
    let test_positions = create_test_positions();

    // Benchmark Material evaluator
    let material_result = benchmark_evaluator("Material", &MaterialEvaluator, &test_positions);

    // Benchmark NNUE evaluator
    let nnue_evaluator = NNUEEvaluatorWrapper::zero();
    let nnue_result = benchmark_evaluator("NNUE", &nnue_evaluator, &test_positions);

    let nnue_slowdown_factor =
        material_result.evals_per_sec as f64 / nnue_result.evals_per_sec as f64;

    ComparisonResult {
        material: material_result,
        nnue: nnue_result,
        nnue_slowdown_factor,
    }
}

/// Benchmark a specific evaluator
fn benchmark_evaluator<E: Evaluator>(
    name: &str,
    evaluator: &E,
    positions: &[Position],
) -> EvaluationBenchmarkResult {
    println!("  Benchmarking {name} evaluator...");

    let iterations_per_position = 10_000;
    let total_iterations = positions.len() * iterations_per_position;

    let start = Instant::now();

    for pos in positions {
        for _ in 0..iterations_per_position {
            let score = evaluator.evaluate(pos);
            // Prevent optimization
            std::hint::black_box(score);
        }
    }

    let elapsed = start.elapsed();
    let evals_per_sec = (total_iterations as f64 / elapsed.as_secs_f64()) as u64;
    let avg_eval_time_ns = elapsed.as_nanos() as u64 / total_iterations as u64;

    println!("    Evaluations per second: {evals_per_sec}");
    println!("    Average time per eval: {avg_eval_time_ns} ns");

    EvaluationBenchmarkResult {
        name: name.to_string(),
        evals_per_sec,
        avg_eval_time_ns,
        total_evals: total_iterations as u64,
    }
}

/// Create diverse test positions for benchmarking
fn create_test_positions() -> Vec<Position> {
    let mut positions = vec![
        // Starting position
        Position::startpos(),
    ];

    // Middle game position with some pieces captured
    let _midgame = Position::startpos();
    // Simulate some moves to create a middle game position
    // (In a real implementation, we would apply actual moves)

    // Endgame position
    let mut endgame = Position::empty();
    // Add kings
    endgame
        .board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
    endgame
        .board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
    // Add some pieces
    endgame
        .board
        .put_piece(Square::new(5, 7), Piece::new(PieceType::Gold, Color::Black));
    endgame
        .board
        .put_piece(Square::new(3, 1), Piece::new(PieceType::Gold, Color::White));
    endgame
        .board
        .put_piece(Square::new(6, 6), Piece::new(PieceType::Silver, Color::Black));
    endgame
        .board
        .put_piece(Square::new(2, 2), Piece::new(PieceType::Silver, Color::White));
    // Add some pawns
    endgame
        .board
        .put_piece(Square::new(7, 5), Piece::new(PieceType::Pawn, Color::Black));
    endgame
        .board
        .put_piece(Square::new(1, 3), Piece::new(PieceType::Pawn, Color::White));
    positions.push(endgame);

    // Position with pieces in hand
    let mut hand_position = Position::startpos();
    hand_position.hands[Color::Black as usize][6] = 2; // 2 pawns in hand
    hand_position.hands[Color::White as usize][6] = 1; // 1 pawn in hand
    positions.push(hand_position);

    positions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn test_benchmark() {
        let result = run_benchmark();

        // Should achieve reasonable performance
        // Note: Debug builds are much slower than release builds
        // Allow some variance for CI environments (5% tolerance)
        assert!(
            result.movegen_speed > 95_000,
            "Move generation speed: {} moves/sec",
            result.movegen_speed
        ); // At least 95k moves/sec in debug
        assert!(result.nps > 9_500, "NPS: {} nodes/sec", result.nps); // At least 9.5k NPS in debug
    }

    #[test]
    fn test_evaluation_benchmark() {
        let comparison = benchmark_evaluation();

        // Basic sanity checks
        assert!(comparison.material.evals_per_sec > 0);
        assert!(comparison.nnue.evals_per_sec > 0);
        assert!(comparison.material.avg_eval_time_ns > 0);
        assert!(comparison.nnue.avg_eval_time_ns > 0);

        // NNUE should be slower than simple material evaluation
        assert!(comparison.nnue_slowdown_factor > 1.0);

        // But not unreasonably slow
        // In debug mode, NNUE can be 200x+ slower due to lack of optimizations
        #[cfg(debug_assertions)]
        assert!(comparison.nnue_slowdown_factor < 500.0);

        // In release mode, should be much faster
        #[cfg(not(debug_assertions))]
        assert!(comparison.nnue_slowdown_factor < 50.0);
    }

    #[test]
    fn test_create_test_positions() {
        let positions = create_test_positions();

        // Should have at least 3 positions
        assert!(positions.len() >= 3);

        // First position should be startpos
        let startpos = Position::startpos();
        assert_eq!(positions[0].board.piece_bb, startpos.board.piece_bb);

        // Verify endgame position has fewer pieces
        let endgame = &positions[1];
        let endgame_pieces =
            (endgame.board.occupied_bb[0] | endgame.board.occupied_bb[1]).count_ones();
        assert!(endgame_pieces < 40); // Less than starting position

        // Verify hand position has pieces in hand
        let hand_pos = &positions[2];
        assert!(hand_pos.hands[Color::Black as usize][6] > 0);
    }
}

/// Main function for standalone benchmark
#[cfg(not(test))]
pub fn main() {
    let result = run_benchmark();

    println!("\n=== Benchmark Results ===");
    println!("Move Generation: {} moves/sec", result.movegen_speed);
    println!("Search NPS: {} nodes/sec", result.nps);
    println!("Total Nodes: {}", result.nodes);
    println!("Total Time: {:?}", result.elapsed);

    // Run evaluation comparison
    let eval_comparison = benchmark_evaluation();

    println!("\n=== Evaluation Function Comparison ===");
    println!("Material Evaluator:");
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
}
