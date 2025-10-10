//! Regression tests for king_danger_squares optimization around sliding vs. king-like checks.

use crate::movegen::MoveGenerator;
use crate::shogi::{Color, Piece, PieceType};
use crate::usi::{move_to_usi, parse_usi_square, position_to_sfen};
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
    let usi_moves: Vec<String> = moves.as_slice().iter().map(move_to_usi).collect();

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

#[test]
fn bishop_sliding_check_blocks_away_diagonal_escape() {
    // Black king 5e, White bishop 2b gives sliding diagonal check.
    // Moving away along the diagonal (5e->6f) must be illegal,
    // while a non-collinear escape (5e->4f) should be allowed.
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    // Kings
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));
    // White bishop at 2b
    pos.board
        .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Bishop, Color::White));
    pos.hash = pos.compute_hash();
    pos.zobrist_hash = pos.hash;

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("movegen ok");
    let usi_moves: Vec<String> = moves.as_slice().iter().map(move_to_usi).collect();

    assert!(
        !usi_moves.iter().any(|m| m == "5e6f"),
        "Diagonal sliding check must block away-diagonal escape 5e6f. Moves: {:?}",
        usi_moves
    );
    assert!(
        usi_moves.iter().any(|m| m == "5e4f"),
        "Non-collinear escape 5e4f should be allowed. Moves: {:?}",
        usi_moves
    );
}

#[test]
fn rook_sliding_check_blocks_away_linear_escape() {
    // Black king 5e, White rook 5a gives sliding file check.
    // Moving away along the file (5e->5f) must be illegal,
    // while a side escape (5e->4e) should be allowed.
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.hash = pos.compute_hash();
    pos.zobrist_hash = pos.hash;

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("movegen ok");
    let usi_moves: Vec<String> = moves.as_slice().iter().map(move_to_usi).collect();

    assert!(
        !usi_moves.iter().any(|m| m == "5e5f"),
        "Linear sliding check must block away-linear escape 5e5f. Moves: {:?}",
        usi_moves
    );
    assert!(
        usi_moves.iter().any(|m| m == "5e4e"),
        "Side escape 5e4e should be allowed. Moves: {:?}",
        usi_moves
    );
}

#[test]
fn lance_sliding_check_blocks_away_linear_escape() {
    // Black king 5e, White lance 5a (unpromoted) gives sliding file check.
    // Moving away along the file (5e->5f) must be illegal, side 5e4e should be allowed.
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Lance, Color::White));
    pos.hash = pos.compute_hash();
    pos.zobrist_hash = pos.hash;

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("movegen ok");
    let usi_moves: Vec<String> = moves.as_slice().iter().map(move_to_usi).collect();

    assert!(
        !usi_moves.iter().any(|m| m == "5e5f"),
        "Lance sliding check must block away-linear escape 5e5f. Moves: {:?}",
        usi_moves
    );
    assert!(
        usi_moves.iter().any(|m| m == "5e4e"),
        "Side escape 5e4e should be allowed. Moves: {:?}",
        usi_moves
    );
}

#[test]
fn promoted_lance_adjacent_check_allows_side_escape() {
    // Black king 5e, White promoted lance (+L) at 5d gives adjacent (gold-like) check.
    // Side escape 5e4e must remain allowed (no ray extension in this case).
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Lance, Color::White));
    // Promote the lance
    let sq = parse_usi_square("5d").unwrap();
    pos.board.promoted_bb.set(sq);
    pos.hash = pos.compute_hash();
    pos.zobrist_hash = pos.hash;

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("movegen ok");
    let usi_moves: Vec<String> = moves.as_slice().iter().map(move_to_usi).collect();

    // Extension must not apply in +L adjacent check. 5e5f（直線に遠ざかる）や斜め逃げが許されうる。
    // 接近王手（+L=金相当）ではレイ延長は行われない。ここでは 5e5f が許可される形。
    assert!(
        usi_moves.iter().any(|m| m == "5e5f"),
        "+L adjacent check must allow 5e5f. Moves: {:?}",
        usi_moves
    );
}

#[test]
fn double_check_generates_only_king_moves() {
    // Double check: Black king 5e; White rook 5a (file check) + White bishop 1a (diagonal check).
    // 生成される手は自玉の手のみ（ブロック・他駒の手は出ない）。
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    // Kings
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::White));
    // Attackers
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::Bishop, Color::White));
    pos.hash = pos.compute_hash();
    pos.zobrist_hash = pos.hash;

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("movegen ok");
    // All moves must originate from king square 5e
    let king_sq = parse_usi_square("5e").unwrap();
    assert!(
        moves.as_slice().iter().all(|m| m.from() == Some(king_sq)),
        "Double check must generate only king moves. Moves: {:?}",
        moves.as_slice().iter().map(move_to_usi).collect::<Vec<_>>()
    );
}

#[test]
fn has_legal_moves_consistent_with_generated() {
    // In a simple in-check position with one safe escape, has_legal_moves must be true
    // and generate_all must be non-empty.
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.hash = pos.compute_hash();
    pos.zobrist_hash = pos.hash;

    let gen = MoveGenerator::new();
    let any = gen.has_legal_moves(&pos).expect("ok");
    let moves = gen.generate_all(&pos).expect("ok");
    assert!(any, "Expected at least one legal move (king escape)");
    assert!(
        !moves.is_empty(),
        "generate_all should be non-empty when has_legal_moves is true"
    );
}

#[test]
fn dragon_adjacent_diagonal_check_allows_side_escape() {
    // Minimal board: Black king at 5i, White dragon (+R) at 6h giving adjacent diagonal check.
    // The king should be able to escape to 4i (sideways), which must not be filtered out.
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    // Place kings
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    // Place promoted rook (dragon) at 6h (promoted bit is encoded via board API using PieceType::Rook + promoted_bb)
    pos.board
        .put_piece(parse_usi_square("6h").unwrap(), Piece::new(PieceType::Rook, Color::White));
    // Mark promoted (+R)
    let sq_dragon = parse_usi_square("6h").unwrap();
    pos.board.promoted_bb.set(sq_dragon);
    pos.hash = pos.compute_hash();
    pos.zobrist_hash = pos.hash;

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("movegen ok");
    let usi_moves: Vec<String> = moves.as_slice().iter().map(move_to_usi).collect();

    assert!(
        usi_moves.iter().any(|m| m == "5i4i"),
        "Dragon's adjacent diagonal check must allow 5i4i. Moves: {:?}",
        usi_moves
    );
}
