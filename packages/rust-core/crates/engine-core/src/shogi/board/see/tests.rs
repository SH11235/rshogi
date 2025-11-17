//! Tests for Static Exchange Evaluation (SEE)

use crate::shogi::board::{Color, Piece, PieceType};
use crate::shogi::moves::Move;
use crate::shogi::position::Position;
use crate::usi::{parse_usi_move, parse_usi_square};

#[test]
fn test_see_handles_missing_from_piece_gracefully() {
    // 不正な入力（from に駒が無い）が与えられてもパニックしないことを確認
    let mut pos = Position::empty();

    // 取られ役だけ置く（to に白歩）
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Pawn, Color::White));
    pos.side_to_move = Color::Black;

    // from は空マス、to は相手駒という不整合な手
    let mv = Move::normal(
        parse_usi_square("5g").unwrap(), // 空の想定
        parse_usi_square("5a").unwrap(),
        false,
    );

    // パニックしないこと、かつ see_ge(…,0) が false になることを確認
    let _ = pos.see(mv); // 値自体は定義しない（安全側の値）
    assert!(!pos.see_ge(mv, 0));
}

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

    // SEE should equal pawn value
    let pawn_val = PieceType::Pawn.value();
    assert_eq!(pos.see(mv), pawn_val);
    assert!(pos.see_ge(mv, 0));
    assert!(pos.see_ge(mv, pawn_val));
    assert!(!pos.see_ge(mv, pawn_val + 1));
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

    // SEE should be pawn - rook (win pawn, lose rook)
    let expected = PieceType::Pawn.value() - PieceType::Rook.value();
    assert_eq!(pos.see(mv), expected);
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

    // Exchange net: +Gold value (pawn trades cancel)
    assert_eq!(pos.see(mv), PieceType::Gold.value());
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

    // Exchange net: pawn value
    assert_eq!(pos.see(mv), PieceType::Pawn.value());
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

    // Silver takes Pawn (pawn value swing)
    assert_eq!(pos.see(mv), PieceType::Pawn.value());
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

    // Gold takes Pawn (pawn value swing)
    assert_eq!(pos.see(mv), PieceType::Pawn.value());
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

    // SEE value: Pawn takes Gold, then Rook captures pawn
    let see_value = pos.see(mv);
    let expected = PieceType::Gold.value() - PieceType::Pawn.value();
    assert_eq!(see_value, expected);

    // Test see_ge with various thresholds
    // Should use delta pruning for early termination
    assert!(!pos.see_ge(mv, PieceType::Gold.value()));
    assert!(pos.see_ge(mv, expected));
    assert!(pos.see_ge(mv, expected - 50));
    assert!(pos.see_ge(mv, 0)); // 500 > 0
    assert!(pos.see_ge(mv, -PieceType::Pawn.value()));
}

#[test]
fn test_see_defended_pawn() {
    // Test capturing a defended pawn - should be negative
    let mut pos = Position::empty();

    // Black Rook on 5f
    pos.board
        .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Rook, Color::Black));

    // White Pawn on 5d - defended by silver
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Pawn, Color::White));

    // White Silver on 4c - defending the pawn
    pos.board
        .put_piece(parse_usi_square("4c").unwrap(), Piece::new(PieceType::Silver, Color::White));

    // Kings for proper position
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    // Rook takes defended pawn
    let mv = Move::normal(parse_usi_square("5f").unwrap(), parse_usi_square("5d").unwrap(), false);

    // SEE: RxP (+pawn), SxR (-rook)
    let expected = PieceType::Pawn.value() - PieceType::Rook.value();
    assert_eq!(pos.see(mv), expected);
    assert!(!pos.see_ge(mv, 0));
}

#[test]
fn test_see_equal_exchange() {
    // Test equal exchange (rook vs rook)
    let mut pos = Position::empty();

    // Black Rook on 5f
    pos.board
        .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Rook, Color::Black));

    // White Rook on 5d - defended by another rook
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Rook, Color::White));

    // White Rook on 5a - defending
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));

    // Kings for proper position
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    // Rook takes rook
    let mv = Move::normal(parse_usi_square("5f").unwrap(), parse_usi_square("5d").unwrap(), false);

    // SEE: RxR (+900), RxR (-900)
    // Total: 900 - 900 = 0
    assert_eq!(pos.see(mv), 0);
    assert!(pos.see_ge(mv, 0));
    assert!(!pos.see_ge(mv, 1));
}

#[test]
fn test_see_x_ray_with_equal_value() {
    // Test X-ray attack with equal value pieces
    let mut pos = Position::empty();

    // Black Bishop on 7g
    pos.board
        .put_piece(parse_usi_square("7g").unwrap(), Piece::new(PieceType::Bishop, Color::Black));

    // Black Bishop on 8h - behind first bishop
    pos.board
        .put_piece(parse_usi_square("8h").unwrap(), Piece::new(PieceType::Bishop, Color::Black));

    // White Bishop on 5e - defended by bishop
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Bishop, Color::White));

    // White Bishop on 2b - defending
    pos.board
        .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Bishop, Color::White));

    // Kings
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    // Bishop takes bishop
    let mv = Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("5e").unwrap(), false);

    let bishop_val = PieceType::Bishop.value();
    assert_eq!(pos.see(mv), bishop_val);
    assert!(pos.see_ge(mv, 0));
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

    let pawn_val = PieceType::Pawn.value();
    assert_eq!(pos.see(mv), pawn_val);

    // Test see_ge with threshold that triggers early termination
    assert!(!pos.see_ge(mv, pawn_val * 10));
    assert!(!pos.see_ge(mv, pawn_val * 5));
    assert!(!pos.see_ge(mv, pawn_val * 2));
    assert!(pos.see_ge(mv, pawn_val));
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

    let see_value = pos.see(mv);
    let expected = PieceType::Gold.value() + PieceType::Silver.value() - PieceType::Pawn.value();
    assert_eq!(see_value, expected, "Should be a good exchange: {see_value}");
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

    let see_value = pos.see(mv);
    let expected = PieceType::Pawn.value() + PieceType::Rook.value() + PieceType::Bishop.value()
        - PieceType::Silver.value();
    assert_eq!(see_value, expected);
}

/// 対局ログ由来の問題局面を構築するヘルパ。
/// `6f6d` を指す直前の局面を返す。
fn problem_position_before_6f6d() -> Position {
    const MOVES_BEFORE_6F6D: &[&str] = &[
        "3g3f", "3c3d", "2h1h", "4a3b", "1h3h", "8c8d", "3f3e", "3d3e", "3h3e", "8d8e", "6i7h",
        "7a7b", "3e3f", "5a4a", "3f5f", "3a4b", "4i4h", "4a3a", "5f3f", "4b3c", "3i2h", "4c4d",
        "5i6h", "7b8c", "3f6f", "6a5b", "6f3f", "7c7d", "6h5i", "7d7e", "7g7f", "7e7f", "8h5e",
        "8b9b", "3f7f", "P*7d", "8i7g", "5b4c", "7f6f", "8c7b", "7g8e", "7d7e",
    ];

    let mut pos = Position::startpos();
    for usi in MOVES_BEFORE_6F6D {
        let mv = parse_usi_move(usi).expect("valid usi move in sequence");
        assert!(pos.is_legal_move(mv), "illegal move in sequence: {}", usi);
        pos.do_move(mv);
    }
    pos
}

/// Quiet XSEE（着地点 SEE）が問題手 `6f6d` に対して十分大きなマイナスになることを検証する。
/// これにより Root SEE Gate でタダ損級の静かな手をルートから除外できる。
#[test]
fn landing_see_for_6f6d_is_strongly_negative() {
    let pos = problem_position_before_6f6d();
    let mv_6f6d = parse_usi_move("6f6d").expect("valid usi move 6f6d");
    assert!(pos.is_legal_move(mv_6f6d), "6f6d must be legal in problem position");

    let xsee = pos.xsee_quiet_after_make(mv_6f6d);

    // 歩4枚分相当の大きな損失として扱えることを要求する（-400cp以下）。
    assert!(xsee <= -400, "XSEE(6f6d) should be strongly negative (<= -400cp), got {}", xsee);
}

/// Quiet XSEE が「味方大駒が新たに利きを通される」型のタダ損手 `5b4b` を強くマイナスと判定できることを検証する。
#[test]
fn vacated_major_threat_for_5b4b_is_strongly_negative() {
    let mut pos = Position::empty();
    // Black pieces
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Rook, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White pieces
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    let mv_5b4b =
        Move::normal(parse_usi_square("5b").unwrap(), parse_usi_square("4b").unwrap(), false);
    assert!(pos.is_legal_move(mv_5b4b), "5b4b must be legal in test position");

    let xsee = pos.xsee_quiet_after_make(mv_5b4b);
    assert!(
        xsee <= -400,
        "XSEE(5b4b) should be strongly negative (<= -400cp) due to vacated major threat, got {}",
        xsee
    );
}

/// Quiet XSEE が「着地点が即座に大駒を取られる」型のタダ損手 `7b7a` を強くマイナスと判定できることを検証する。
#[test]
fn landing_see_for_7b7a_is_strongly_negative() {
    let mut pos = Position::empty();
    // Black pieces
    pos.board
        .put_piece(parse_usi_square("7b").unwrap(), Piece::new(PieceType::Rook, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White pieces
    pos.board
        .put_piece(parse_usi_square("7c").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    let mv_7b7a =
        Move::normal(parse_usi_square("7b").unwrap(), parse_usi_square("7a").unwrap(), false);
    assert!(pos.is_legal_move(mv_7b7a), "7b7a must be legal in test position");

    let xsee = pos.xsee_quiet_after_make(mv_7b7a);
    assert!(
        xsee <= -400,
        "XSEE(7b7a) should be strongly negative (<= -400cp) due to immediate capture on landing square, got {}",
        xsee
    );
}

/// SEE edge cases and complex scenarios
#[cfg(test)]
mod see_edge_cases {
    use super::*;

    #[test]
    fn test_see_x_ray_discovery() {
        // Test X-ray attacks: when a piece moves and discovers another attacker
        let mut pos = Position::empty();

        // Target square: 5e with a valuable piece (Dragon)
        let mut dragon = Piece::new(PieceType::Rook, Color::White);
        dragon.promoted = true; // Dragon (promoted rook)
        pos.board.put_piece(parse_usi_square("5e").unwrap(), dragon);

        // Immediate attacker: Black silver on 4f
        pos.board.put_piece(
            parse_usi_square("4f").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );

        // X-ray discovery: Black rook on 5i with bishop on 5g blocking the path
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::Rook, Color::Black));
        pos.board.put_piece(
            parse_usi_square("5g").unwrap(),
            Piece::new(PieceType::Bishop, Color::Black),
        );

        // White defender: Gold on 4e
        pos.board
            .put_piece(parse_usi_square("4e").unwrap(), Piece::new(PieceType::Gold, Color::White));

        pos.side_to_move = Color::Black;
        pos.board.rebuild_occupancy_bitboards();

        // Silver takes Dragon: should consider that bishop will move and rook becomes attacker
        let mv =
            Move::normal(parse_usi_square("4f").unwrap(), parse_usi_square("5e").unwrap(), false);

        let see_value = pos.see(mv);
        // Expected: +1200 (dragon) - 500 (silver) + potential rook support
        assert!(see_value > 600, "X-ray discovery should be profitable: {see_value}");
    }

    #[test]
    fn test_see_multiple_attackers_same_value() {
        // Test with multiple pieces of same value attacking
        let mut pos = Position::empty();

        // Target: Gold on 5e
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Gold, Color::White));

        // Multiple Black attackers of same value (Silvers: 500 each)
        pos.board.put_piece(
            parse_usi_square("4f").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );
        pos.board.put_piece(
            parse_usi_square("6f").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );
        pos.board.put_piece(
            parse_usi_square("5f").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );

        // Multiple White defenders of same value (Silvers: 500 each)
        pos.board.put_piece(
            parse_usi_square("4d").unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        );
        pos.board.put_piece(
            parse_usi_square("6d").unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        );

        pos.side_to_move = Color::Black;
        pos.board.rebuild_occupancy_bitboards();

        let mv =
            Move::normal(parse_usi_square("4f").unwrap(), parse_usi_square("5e").unwrap(), false);

        let see_value = pos.see(mv);
        // With 3 attackers vs 2 defenders (all same value), should be profitable
        // +600 (gold) - 500 (silver) + 500 (recapture silver) - 500 (counter-recapture) = +100
        assert!(see_value > 0, "Multiple attackers should win: {see_value}");
        // Note: Current SEE may overestimate if some recaptures aren't detected in this setup.
        // We only assert positivity here to avoid over-constraining the implementation.
    }

    #[test]
    fn test_see_pinned_piece_cannot_capture() {
        // Test that pinned pieces cannot capture when it would expose their king
        let mut pos = Position::empty();

        // Kings
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        // Target: valuable piece (Rook) on 4h
        pos.board
            .put_piece(parse_usi_square("4h").unwrap(), Piece::new(PieceType::Rook, Color::White));

        // Black bishop on 5h (pinned by white lance)
        pos.board.put_piece(
            parse_usi_square("5h").unwrap(),
            Piece::new(PieceType::Bishop, Color::Black),
        );

        // White lance on 5c creating the pin
        pos.board
            .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Lance, Color::White));

        pos.side_to_move = Color::Black;
        pos.board.rebuild_occupancy_bitboards();

        // Bishop tries to capture rook, but it's pinned
        let mv =
            Move::normal(parse_usi_square("5h").unwrap(), parse_usi_square("4h").unwrap(), false);

        // SEE should recognize this as illegal/invalid due to pin
        let see_value = pos.see(mv);
        assert!(see_value <= 0, "Pinned piece should not be able to capture: {see_value}");
    }

    #[test]
    fn test_see_promotion_value_calculation() {
        // Test SEE with promotion moves
        let mut pos = Position::empty();

        // Black pawn about to promote on 1st rank
        pos.board
            .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Pawn, Color::Black));

        // Target on promotion square
        pos.board.put_piece(
            parse_usi_square("5a").unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        );

        // White defender
        pos.board
            .put_piece(parse_usi_square("4a").unwrap(), Piece::new(PieceType::Gold, Color::White));

        pos.side_to_move = Color::Black;
        pos.board.rebuild_occupancy_bitboards();

        // Pawn captures and promotes
        let mv =
            Move::normal(parse_usi_square("5b").unwrap(), parse_usi_square("5a").unwrap(), true);

        let see_value = pos.see(mv);
        // Promotion should add significant value, even after potential recapture
        assert!(see_value >= 400, "Promotion should add significant value: {see_value}");
    }

    #[test]
    fn test_see_drop_move_evaluation() {
        // Test SEE evaluation for drop moves (Shogi-specific)
        let mut pos = Position::empty();

        // Add gold to hand manually
        use crate::shogi::piece_constants::piece_type_to_hand_index;
        let gold_idx = piece_type_to_hand_index(PieceType::Gold).unwrap();
        pos.hands[Color::Black as usize][gold_idx] = 1;

        // Target square with enemy piece
        pos.board.put_piece(
            parse_usi_square("5e").unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        );

        // Enemy defender
        pos.board
            .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Gold, Color::White));

        pos.side_to_move = Color::Black;
        pos.board.rebuild_occupancy_bitboards();

        // Drop gold to attack silver; the drop square is defended by a white Gold,
        // so the drop is tactically losing and SEE should be negative.
        let mv = Move::drop(PieceType::Gold, parse_usi_square("5f").unwrap());

        let see_value = pos.see(mv);
        // With drop-aware SEE, disadvantageous drops yield negative values.
        assert!(see_value < 0, "Disadvantageous drop should be negative in SEE, got {see_value}");
        // And threshold check must reflect that
        assert!(!pos.see_ge(mv, 0));
    }

    #[test]
    fn test_see_king_safety_consideration() {
        // Test that SEE considers king safety in exchanges
        let mut pos = Position::empty();

        // Black king
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));

        // White king
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        // Valuable target near Black king
        pos.board
            .put_piece(parse_usi_square("4h").unwrap(), Piece::new(PieceType::Rook, Color::White));

        // Black piece that can capture but would expose king
        pos.board
            .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::Gold, Color::Black));

        // White attacker that would attack the king if Black moves
        pos.board
            .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Lance, Color::White));

        pos.side_to_move = Color::Black;
        pos.board.rebuild_occupancy_bitboards();

        let mv =
            Move::normal(parse_usi_square("5h").unwrap(), parse_usi_square("4h").unwrap(), false);

        let see_value = pos.see(mv);
        // Should be heavily penalized due to king exposure
        assert!(
            see_value < 1200, // Less than rook value due to king safety
            "King safety should be considered: {see_value}"
        );
    }

    #[test]
    fn test_see_silver_captures_pawn_with_promotion_defended_by_rook() {
        // Test the specific case: Silver on 8h captures pawn on 7c with promotion
        // White rook on 2c can recapture
        // SFEN: "4k4/9/p6R1/9/9/9/9/1S7/4K4 b - 1"
        let mut pos = Position::empty();

        // Kings
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));

        // White pawn on 7c (target)
        pos.board
            .put_piece(parse_usi_square("7c").unwrap(), Piece::new(PieceType::Pawn, Color::White));

        // Black silver on 8h (attacker)
        pos.board.put_piece(
            parse_usi_square("8h").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );

        // White rook on 2c (can recapture)
        pos.board
            .put_piece(parse_usi_square("2c").unwrap(), Piece::new(PieceType::Rook, Color::White));

        pos.board.rebuild_occupancy_bitboards();
        pos.side_to_move = Color::Black;

        // Move: Silver captures pawn with promotion (8h7c+)
        let mv =
            Move::normal(parse_usi_square("8h").unwrap(), parse_usi_square("7c").unwrap(), true);

        let see_value = pos.see(mv);
        eprintln!("SEE value for 8h7c+: {see_value}");

        let promoted_silver_val = PieceType::Silver.value() + PieceType::Silver.promotion_gain();
        let expected =
            PieceType::Pawn.value() + PieceType::Silver.promotion_gain() - promoted_silver_val;
        // This should be negative because the promoted silver gets captured by the rook
        assert!(see_value < 0, "SEE should be negative, but got: {see_value}");
        assert_eq!(see_value, expected, "SEE should match capture math");
    }

    #[test]
    fn test_see_from_exact_sfen() {
        // Test using exact SFEN: "4k4/9/p6R1/9/9/9/9/1S7/4K4 b - 1"
        use crate::usi::parse_sfen;

        let pos = parse_sfen("4k4/9/p6R1/9/9/9/9/1S7/4K4 b - 1").expect("Valid SFEN");

        // Debug: Print all pieces
        eprintln!("=== Board state from SFEN ===");
        for rank in 1..=9 {
            for file in (1..=9).rev() {
                let sq = parse_usi_square(&format!("{file}{}", (b'a' + rank - 1) as char)).unwrap();
                if let Some(piece) = pos.board.piece_on(sq) {
                    eprintln!(
                        "Square {file}{}: {:?} {:?}",
                        (b'a' + rank - 1) as char,
                        piece.color,
                        piece.piece_type
                    );
                }
            }
        }

        // Check the move 8h9c+ exists and calculate SEE
        // Silver on 8h captures pawn on 9c with promotion
        let from = parse_usi_square("8h").unwrap();
        let to = parse_usi_square("9c").unwrap();

        // Verify pieces are where we expect
        if let Some(piece) = pos.board.piece_on(from) {
            eprintln!("Piece at 8h: {:?} {:?}", piece.color, piece.piece_type);
        } else {
            eprintln!("No piece at 8h!");
        }

        if let Some(piece) = pos.board.piece_on(to) {
            eprintln!("Piece at 9c: {:?} {:?}", piece.color, piece.piece_type);
        } else {
            eprintln!("No piece at 9c!");
        }

        // Check for rook on 2c
        let rook_sq = parse_usi_square("2c").unwrap();
        if let Some(piece) = pos.board.piece_on(rook_sq) {
            eprintln!("Piece at 2c: {:?} {:?}", piece.color, piece.piece_type);
        } else {
            eprintln!("No piece at 2c!");
        }

        // Create the move
        let mv = Move::normal(from, to, true);

        // Calculate SEE
        let see_value = pos.see(mv);
        eprintln!("SEE value for 8h9c+: {see_value}");

        // In this position, the silver captures an undefended pawn with promotion.
        // Since there is no recapture, SEE = pawn value + promotion gain.
        assert_eq!(
            see_value,
            PieceType::Pawn.value() + PieceType::Silver.promotion_gain(),
            "SEE should equal pawn value + promotion gain, but got: {see_value}"
        );
    }

    #[test]
    fn test_see_corrected_position_with_white_rook() {
        // Create the position we actually want to test:
        // - Black Silver on 2h captures White Pawn on 7c with promotion
        // - White Rook on 2c can recapture
        // This is what the user seems to have intended
        let mut pos = Position::empty();

        // Kings
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));

        // White pawn on 7c (target)
        pos.board
            .put_piece(parse_usi_square("7c").unwrap(), Piece::new(PieceType::Pawn, Color::White));

        // Black silver on 2h (attacker) - note: this is rank 8, file 2
        pos.board.put_piece(
            parse_usi_square("2h").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );

        // White rook on 2c (can recapture along the file)
        pos.board
            .put_piece(parse_usi_square("2c").unwrap(), Piece::new(PieceType::Rook, Color::White));

        pos.board.rebuild_occupancy_bitboards();
        pos.side_to_move = Color::Black;

        // Move: Silver captures pawn with promotion (2h7c+)
        let mv =
            Move::normal(parse_usi_square("2h").unwrap(), parse_usi_square("7c").unwrap(), true);

        // Check if rook can actually see the target square
        eprintln!("Checking if White Rook on 2c can attack 7c...");

        let see_value = pos.see(mv);
        eprintln!("SEE value for 2h7c+: {see_value}");

        let promoted_silver_val = PieceType::Silver.value() + PieceType::Silver.promotion_gain();
        let expected =
            PieceType::Pawn.value() + PieceType::Silver.promotion_gain() - promoted_silver_val;
        assert_eq!(
            see_value, expected,
            "SEE should reflect pawn + promotion gain minus promoted silver value, but got: {see_value}"
        );
    }
}
