//! Tests for Static Exchange Evaluation (SEE)

use crate::shogi::board::{Color, Piece, PieceType, Position};
use crate::shogi::moves::Move;
use crate::usi::parse_usi_square;

#[test]
fn test_see_simple_pawn_capture() {
    // Test simple pawn takes pawn
    let mut pos = Position::empty();

    // Black pawn on 5e (parse_usi_square("5e").unwrap())
    let black_pawn = Piece::new(PieceType::Pawn, Color::Black);
    pos.board.put_piece(parse_usi_square("5e").unwrap(), black_pawn);

    // White pawn on 5d
    let white_pawn = Piece::new(PieceType::Pawn, Color::White);
    pos.board.put_piece(parse_usi_square("5d").unwrap(), white_pawn);

    // Black to move, pawn takes pawn
    pos.side_to_move = Color::Black;
    let mv = Move::normal(parse_usi_square("5e").unwrap(), parse_usi_square("5d").unwrap(), false);

    // SEE should be 100 (pawn value)
    assert_eq!(pos.see(mv), 100);
    assert!(pos.see_ge(mv, 0));
    assert!(pos.see_ge(mv, 100));
    assert!(!pos.see_ge(mv, 101));
}

#[test]
fn test_see_bad_exchange() {
    // Test rook takes pawn defended by pawn
    let mut pos = Position::empty();

    // Black rook on 5f (parse_usi_square("5f").unwrap())
    let black_rook = Piece::new(PieceType::Rook, Color::Black);
    pos.board.put_piece(parse_usi_square("5f").unwrap(), black_rook);

    // White pawn on 5d
    let white_pawn = Piece::new(PieceType::Pawn, Color::White);
    pos.board.put_piece(parse_usi_square("5d").unwrap(), white_pawn);

    // White gold on 5c defending
    let white_gold = Piece::new(PieceType::Gold, Color::White);
    pos.board.put_piece(parse_usi_square("5c").unwrap(), white_gold);

    // Black to move, rook takes pawn
    pos.side_to_move = Color::Black;
    let mv = Move::normal(parse_usi_square("5f").unwrap(), parse_usi_square("5d").unwrap(), false);

    // SEE should be 100 - 900 = -800 (win pawn, lose rook to gold)
    assert_eq!(pos.see(mv), -800);
    assert!(!pos.see_ge(mv, 0));
}

#[test]
fn test_see_complex_exchange() {
    // Test complex exchange: pawn takes pawn, gold takes pawn, silver takes gold
    let mut pos = Position::empty();

    // Black pawn on 5e
    let black_pawn = Piece::new(PieceType::Pawn, Color::Black);
    pos.board.put_piece(parse_usi_square("5e").unwrap(), black_pawn);

    // White pawn on 5d
    let white_pawn = Piece::new(PieceType::Pawn, Color::White);
    pos.board.put_piece(parse_usi_square("5d").unwrap(), white_pawn);

    // White gold on 5c (can capture on 5d)
    let white_gold = Piece::new(PieceType::Gold, Color::White);
    pos.board.put_piece(parse_usi_square("5c").unwrap(), white_gold);

    // Black silver on 4e (can capture on 5d diagonally)
    let black_silver = Piece::new(PieceType::Silver, Color::Black);
    pos.board.put_piece(parse_usi_square("4e").unwrap(), black_silver);

    // Black to move, pawn takes pawn
    pos.side_to_move = Color::Black;
    let mv = Move::normal(parse_usi_square("5e").unwrap(), parse_usi_square("5d").unwrap(), false);

    // Exchange: PxP (win 100), GxP (lose 100), SxG (win 600)
    // Net: 100 - 100 + 600 = 600
    assert_eq!(pos.see(mv), 600);
    assert!(pos.see_ge(mv, 0));
}

#[test]
fn test_see_x_ray_attack() {
    // Test X-ray attack: rook behind rook
    let mut pos = Position::empty();

    // Black rook on 5f
    let black_rook1 = Piece::new(PieceType::Rook, Color::Black);
    pos.board.put_piece(parse_usi_square("5f").unwrap(), black_rook1);

    // Black rook on 5g (behind first rook)
    let black_rook2 = Piece::new(PieceType::Rook, Color::Black);
    pos.board.put_piece(parse_usi_square("5g").unwrap(), black_rook2);

    // White pawn on 5d
    let white_pawn = Piece::new(PieceType::Pawn, Color::White);
    pos.board.put_piece(parse_usi_square("5d").unwrap(), white_pawn);

    // White rook on 5a (defending)
    let white_rook = Piece::new(PieceType::Rook, Color::White);
    pos.board.put_piece(parse_usi_square("5a").unwrap(), white_rook);

    // Black to move, rook takes pawn
    pos.side_to_move = Color::Black;
    let mv = Move::normal(parse_usi_square("5f").unwrap(), parse_usi_square("5d").unwrap(), false);

    // Exchange: RxP (win 100), RxR (lose 900), RxR (win 900)
    // Net: 100 - 900 + 900 = 100
    assert_eq!(pos.see(mv), 100);
    assert!(pos.see_ge(mv, 0));
}

#[test]
fn test_see_with_pinned_piece() {
    // Test SEE with pinned pieces
    let mut pos = Position::empty();

    // Black King at 5i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));

    // Black Gold at 5e - will be pinned
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // White Rook at 5a - pinning the Gold
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));

    // White Pawn at 4e - can be captured
    pos.board
        .put_piece(parse_usi_square("4e").unwrap(), Piece::new(PieceType::Pawn, Color::White));

    // Black Silver at 6f - can capture the pawn
    pos.board
        .put_piece(parse_usi_square("6f").unwrap(), Piece::new(PieceType::Silver, Color::Black));

    // White King at 9a
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    // The Gold cannot capture the Pawn because it's pinned
    // Only the Silver can capture
    let mv = Move::normal(parse_usi_square("6f").unwrap(), parse_usi_square("4e").unwrap(), false); // Silver takes Pawn

    // Silver takes Pawn (+100)
    assert_eq!(pos.see(mv), 100);
}

#[test]
fn test_see_with_diagonal_pin() {
    // Test SEE with diagonally pinned piece
    let mut pos = Position::empty();

    // Black King at 5i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));

    // Black Silver at 4h - will be pinned diagonally
    pos.board
        .put_piece(parse_usi_square("4h").unwrap(), Piece::new(PieceType::Silver, Color::Black));

    // White Bishop at 1e - pinning the Silver
    pos.board
        .put_piece(parse_usi_square("1e").unwrap(), Piece::new(PieceType::Bishop, Color::White));

    // White Pawn at 3h - Silver cannot capture due to pin
    pos.board
        .put_piece(parse_usi_square("3h").unwrap(), Piece::new(PieceType::Pawn, Color::White));

    // Black Gold at 3g - can capture the pawn
    pos.board
        .put_piece(parse_usi_square("3g").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // White King at 9a
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    // The Silver is pinned and cannot capture
    // Only the Gold can capture
    let mv = Move::normal(parse_usi_square("3g").unwrap(), parse_usi_square("3h").unwrap(), false); // Gold takes Pawn

    // Gold takes Pawn (+100)
    assert_eq!(pos.see(mv), 100);
}

#[test]
fn test_see_delta_pruning() {
    // Test delta pruning optimization in SEE
    let mut pos = Position::empty();

    // Set up a position where delta pruning can help
    // Black King at 5a
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White King at 5i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black Pawn at 5d
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
    // White Gold at 5e (defended by Rook)
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Gold, Color::White));
    // White Rook at 5g
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Rook, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    let mv = Move::normal(parse_usi_square("5d").unwrap(), parse_usi_square("5e").unwrap(), false); // Pawn takes Gold

    // SEE value: Pawn takes Gold (+600), Rook takes Pawn (-100)
    let see_value = pos.see(mv);
    // Total: +600 - 100 = +500
    assert_eq!(see_value, 500);

    // Test see_ge with various thresholds
    // Should use delta pruning for early termination
    assert!(!pos.see_ge(mv, 600)); // 500 < 600
    assert!(pos.see_ge(mv, 500)); // 500 >= 500
    assert!(pos.see_ge(mv, 400)); // 500 > 400
    assert!(pos.see_ge(mv, 0)); // 500 > 0
    assert!(pos.see_ge(mv, -100)); // 500 > -100
}

#[test]
fn test_see_ge_early_termination() {
    // Test that see_ge can terminate early when threshold cannot be reached
    let mut pos = Position::empty();

    // Black King at 5a
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White King at 5i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black Pawn at 5d
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
    // White Pawn at 5e (undefended)
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Pawn, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    let mv = Move::normal(parse_usi_square("5d").unwrap(), parse_usi_square("5e").unwrap(), false); // Pawn takes Pawn

    // Normal SEE value is +100 (simple pawn capture)
    assert_eq!(pos.see(mv), 100);

    // Test see_ge with threshold that triggers early termination
    assert!(!pos.see_ge(mv, 1000)); // Can't reach 1000 with just a pawn capture
    assert!(!pos.see_ge(mv, 500)); // Can't reach 500 either
    assert!(!pos.see_ge(mv, 200)); // Can't reach 200
    assert!(pos.see_ge(mv, 100)); // Exactly 100
    assert!(pos.see_ge(mv, 0)); // Greater than 0
}

#[test]
fn test_see_multiple_high_value_attackers() {
    // Test case with multiple high-value pieces (Rook + Bishop + Lance)
    let mut pos = Position::empty();

    // Set up a simple position where Black has multiple attackers
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::White));

    // Target: White Gold on 5e worth 600
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Gold, Color::White));

    // Black attackers:
    // - Pawn on 5d (can take Gold)
    // - Rook on 5i (can support after pawn takes)
    // - Bishop on 8b (can support after pawn takes)
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::Rook, Color::Black));
    pos.board
        .put_piece(parse_usi_square("8b").unwrap(), Piece::new(PieceType::Bishop, Color::Black));

    // White defender: Silver on 4d
    pos.board
        .put_piece(parse_usi_square("4d").unwrap(), Piece::new(PieceType::Silver, Color::White));

    pos.side_to_move = Color::Black;

    // Move: Pawn takes Gold
    let mv = Move::normal(parse_usi_square("5d").unwrap(), parse_usi_square("5e").unwrap(), false);

    // SEE calculation:
    // +600 (gold) - 100 (pawn) + 500 (silver) - 700 (bishop) = 300
    // But actually the exchange ends with +600 - 100 = 500 since White will not take the pawn
    // with Silver if it loses material
    let see_value = pos.see(mv);
    assert!(see_value > 0, "Should be a good exchange: {see_value}");

    // Test that see_ge works correctly with multiple attackers
    // The key test is that the algorithm considers all remaining attackers
    assert!(pos.see_ge(mv, 0)); // Positive value
    assert!(pos.see_ge(mv, 500)); // Can reach 500
    assert!(!pos.see_ge(mv, 1500)); // Cannot reach 1500
}

#[test]
fn test_see_promoted_pieces() {
    // Test SEE with promoted pieces to ensure correct value calculation
    let mut pos = Position::empty();

    // Kings
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::White));

    // Target: White promoted pawn (Tokin) on 5e
    let mut tokin = Piece::new(PieceType::Pawn, Color::White);
    tokin.promoted = true;
    pos.board.put_piece(parse_usi_square("5e").unwrap(), tokin);

    // Black attacker: Silver on 6d
    pos.board
        .put_piece(parse_usi_square("6d").unwrap(), Piece::new(PieceType::Silver, Color::Black));

    // White defenders: promoted Rook (Dragon) on 5i that can recapture
    let mut dragon = Piece::new(PieceType::Rook, Color::White);
    dragon.promoted = true;
    pos.board.put_piece(parse_usi_square("5i").unwrap(), dragon);

    // Black has another attacker: promoted Bishop (Horse) on 8h
    let mut horse = Piece::new(PieceType::Bishop, Color::Black);
    horse.promoted = true;
    pos.board.put_piece(parse_usi_square("8h").unwrap(), horse);

    pos.side_to_move = Color::Black;

    // Move: Silver takes Tokin
    let mv = Move::normal(parse_usi_square("6d").unwrap(), parse_usi_square("5e").unwrap(), false);

    // SEE calculation:
    // +600 (tokin) - 500 (silver) + 1200 (dragon) - 900 (horse) = 400
    // But White won't take if it loses material, so it's just +600 - 500 = 100
    let see_value = pos.see(mv);
    assert!(see_value > 0, "Should be a good exchange: {see_value}");

    // Test that the algorithm correctly sums multiple promoted pieces
    assert!(pos.see_ge(mv, 0)); // Positive value
    assert!(pos.see_ge(mv, 100)); // Exactly 100
    assert!(!pos.see_ge(mv, 200)); // Cannot reach 200
}
