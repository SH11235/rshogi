use engine_core::shogi::{Color, PieceType, Position, Square};

#[test]
fn test_usi_coordinate_conversion() {
    // Test USI coordinate conversion
    println!("Testing USI coordinate conversion:");

    // Test file 2 (where Rook starts)
    let sq_2h = Square::from_usi_chars('2', 'h').unwrap();
    assert_eq!(sq_2h.file(), 7); // Internal file 7 = USI file 2
    assert_eq!(sq_2h.rank(), 7); // Rank h = 7
    assert_eq!(sq_2h.to_string(), "2h");

    // Test file 8 (where Bishop starts)
    let sq_8h = Square::from_usi_chars('8', 'h').unwrap();
    assert_eq!(sq_8h.file(), 1); // Internal file 1 = USI file 8
    assert_eq!(sq_8h.rank(), 7); // Rank h = 7
    assert_eq!(sq_8h.to_string(), "8h");

    // Test move 2g2f
    let from = Square::from_usi_chars('2', 'g').unwrap();
    let to = Square::from_usi_chars('2', 'f').unwrap();
    assert_eq!(from.file(), 7); // Both on file 2 (internal 7)
    assert_eq!(to.file(), 7);
    assert_eq!(from.rank(), 6); // g = 6
    assert_eq!(to.rank(), 5); // f = 5
    assert_eq!(from.to_string(), "2g");
    assert_eq!(to.to_string(), "2f");
}

#[test]
fn test_initial_position_pieces() {
    let pos = Position::startpos();

    // Check Black Rook at 2h
    let rook_sq = Square::from_usi_chars('2', 'h').unwrap();
    let piece = pos.piece_at(rook_sq).expect("Should have piece at 2h");
    assert_eq!(piece.piece_type, PieceType::Rook);
    assert_eq!(piece.color, Color::Black);

    // Check Black Bishop at 8h
    let bishop_sq = Square::from_usi_chars('8', 'h').unwrap();
    let piece = pos.piece_at(bishop_sq).expect("Should have piece at 8h");
    assert_eq!(piece.piece_type, PieceType::Bishop);
    assert_eq!(piece.color, Color::Black);

    // Check White Rook at 8b
    let white_rook_sq = Square::from_usi_chars('8', 'b').unwrap();
    let piece = pos.piece_at(white_rook_sq).expect("Should have piece at 8b");
    assert_eq!(piece.piece_type, PieceType::Rook);
    assert_eq!(piece.color, Color::White);

    // Check White Bishop at 2b
    let white_bishop_sq = Square::from_usi_chars('2', 'b').unwrap();
    let piece = pos.piece_at(white_bishop_sq).expect("Should have piece at 2b");
    assert_eq!(piece.piece_type, PieceType::Bishop);
    assert_eq!(piece.color, Color::White);
}

#[test]
fn test_file_conversion_full_range() {
    // Test all files 1-9
    let test_cases = vec![
        ('1', 8), // Rightmost file
        ('2', 7),
        ('3', 6),
        ('4', 5),
        ('5', 4), // Center file
        ('6', 3),
        ('7', 2),
        ('8', 1),
        ('9', 0), // Leftmost file
    ];

    for (usi_file, internal_file) in test_cases {
        let sq = Square::from_usi_chars(usi_file, 'e').unwrap();
        assert_eq!(
            sq.file(),
            internal_file,
            "USI file {usi_file} should map to internal file {internal_file}"
        );
        assert_eq!(
            sq.to_string(),
            format!("{usi_file}e"),
            "Round-trip conversion should preserve USI notation"
        );
    }
}
