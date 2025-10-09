//! Regression tests for king_danger_squares optimization around sliding vs. king-like checks.

use crate::movegen::MoveGenerator;
use crate::usi::{move_to_usi, position_to_sfen};
use crate::Position;

#[test]
fn horse_adjacent_check_allows_king_escape_6i5i() {
    // Repro from production logs (after ... 4f7i+):
    // In this position, Black is in check by a Horse (+B) adjacent to the king.
    // The legal escape 6i5i must be generated.
    // Previously, king_danger_squares incorrectly marked 5i as forbidden by
    // treating the check as if it were a sliding (diagonal) ray.
    let sfen = "l5knl/1r2g1gb1/psnpp1spp/2p2pp2/P7P/2+pPP3L/1n4PP1/4G1R2/1s+bK1G1N1 b Psl2p 61";
    let pos = Position::from_sfen(sfen).expect("valid SFEN");

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("move generation must succeed");

    // Collect USI strings for readability on failure
    let usi_moves: Vec<String> = moves.as_slice().iter().map(|m| move_to_usi(m)).collect();

    assert!(
        usi_moves.iter().any(|m| m == "6i5i"),
        "6i5i must be legal in this position.\nSFEN: {}\nMoves: {:?}",
        position_to_sfen(&pos),
        usi_moves
    );

    // YaneuraOu perft(1) reported exactly 1 legal move in this position.
    // Assert the count matches to prevent future regressions.
    assert_eq!(moves.len(), 1, "Expected exactly 1 legal move (6i5i). Moves: {:?}", usi_moves);
}
