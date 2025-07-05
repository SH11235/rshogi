#[cfg(test)]
mod sfen_parser_tests {
    use shogi_core::opening_book::*;

    #[test]
    fn test_parse_header_line() {
        let mut parser = SfenParser::new();
        let result = parser.parse_line("#YANEURAOU-DB2016 1.00");

        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Header should not return an entry
    }

    #[test]
    fn test_parse_position_line() {
        let mut parser = SfenParser::new();
        let line = "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let result = parser.parse_line(line);

        assert!(result.is_ok());
        // Should start a new position but not return completed entry yet
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_parse_move_line() {
        let mut parser = SfenParser::new();

        // First set up a position
        parser
            .parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();

        // Then parse a move
        let result = parser.parse_line("7g7f none 50 2 1000");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Move added but entry not complete
    }

    #[test]
    fn test_complete_entry_on_empty_line() {
        let mut parser = SfenParser::new();

        // Set up position
        parser
            .parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();

        // Add move
        parser.parse_line("7g7f none 50 2 1000").unwrap();

        // Empty line should complete the entry
        let result = parser.parse_line("");
        assert!(result.is_ok());
        let entry = result.unwrap();
        assert!(entry.is_some());

        let entry = entry.unwrap();
        assert_eq!(
            entry.position,
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL"
        );
        assert_eq!(entry.turn, 'b');
        assert_eq!(entry.hand, "-");
        assert_eq!(entry.move_count, 1);
        assert_eq!(entry.moves.len(), 1);
        assert_eq!(entry.moves[0].move_notation, "7g7f");
    }

    #[test]
    fn test_parse_complex_position() {
        let mut parser = SfenParser::new();
        let complex_sfen =
            "sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0";

        let result = parser.parse_line(complex_sfen);
        assert!(result.is_ok());

        // Complete the entry
        let result = parser.parse_line("");
        assert!(result.is_ok());
        let entry = result.unwrap().unwrap();

        assert_eq!(
            entry.position,
            "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R"
        );
        assert_eq!(entry.turn, 'b');
        assert_eq!(entry.hand, "NLP2sl2p");
        assert_eq!(entry.move_count, 0);
    }

    #[test]
    fn test_parse_multiple_moves() {
        let mut parser = SfenParser::new();

        parser
            .parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        parser.parse_line("7g7f none 50 2 1000").unwrap();
        parser.parse_line("3c3d none 45 2 800").unwrap();
        parser.parse_line("P*5f none 20 1 500").unwrap();

        let result = parser.parse_line("");
        let entry = result.unwrap().unwrap();

        assert_eq!(entry.moves.len(), 3);
        assert_eq!(entry.moves[0].move_notation, "7g7f");
        assert_eq!(entry.moves[1].move_notation, "3c3d");
        assert_eq!(entry.moves[2].move_notation, "P*5f");
    }

    #[test]
    fn test_invalid_position_line() {
        let mut parser = SfenParser::new();
        let result = parser.parse_line("sfen invalid_format");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_move_line() {
        let mut parser = SfenParser::new();
        parser
            .parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();

        let result = parser.parse_line("invalid move format");
        assert!(result.is_err());
    }

    #[test]
    fn test_parser_reset_on_new_position() {
        let mut parser = SfenParser::new();

        // First entry
        parser
            .parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        parser.parse_line("7g7f none 50 2 1000").unwrap();

        // Start new entry - should return the completed first entry
        let result = parser.parse_line(
            "sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0",
        );
        assert!(result.is_ok());
        
        let first_entry = result.unwrap();
        assert!(first_entry.is_some());
        let entry = first_entry.unwrap();
        assert_eq!(entry.moves.len(), 1);
        assert_eq!(entry.moves[0].move_notation, "7g7f");
    }

    #[test]
    fn test_parse_move_with_different_evaluations() {
        let mut parser = SfenParser::new();

        parser
            .parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        parser.parse_line("7g7f none 100 3 2000").unwrap();
        parser.parse_line("2g2f none -50 1 500").unwrap();
        parser.parse_line("P*5f none 0 0 0").unwrap();

        let result = parser.parse_line("");
        let entry = result.unwrap().unwrap();

        assert_eq!(entry.moves.len(), 3);
        assert_eq!(entry.moves[0].evaluation, 100);
        assert_eq!(entry.moves[1].evaluation, -50);
        assert_eq!(entry.moves[2].evaluation, 0);
        assert_eq!(entry.moves[0].depth, 3);
        assert_eq!(entry.moves[1].depth, 1);
        assert_eq!(entry.moves[2].depth, 0);
    }

    #[test]
    fn test_continuous_positions_without_empty_lines() {
        // This test simulates the actual YaneuraOu format where positions
        // are not separated by empty lines
        let mut parser = SfenParser::new();
        let mut entries = Vec::new();

        let lines = vec![
            "#YANEURAOU-DB2016 1.00",
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "7g7f none 50 10 100000",
            "2g2f none 45 10 95000",
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
            "3c3d none -45 10 98000",
            "8c8d none -40 9 93000",
            "sfen lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3",
            "2g2f none 55 11 110000",
        ];

        for line in lines {
            if let Ok(Some(entry)) = parser.parse_line(line) {
                entries.push(entry);
            }
        }

        // Get the last entry
        if let Ok(Some(entry)) = parser.parse_line("") {
            entries.push(entry);
        }

        // Should have parsed 3 positions
        assert_eq!(entries.len(), 3);
        
        // First position
        assert_eq!(entries[0].move_count, 1);
        assert_eq!(entries[0].moves.len(), 2);
        assert_eq!(entries[0].moves[0].move_notation, "7g7f");
        assert_eq!(entries[0].moves[1].move_notation, "2g2f");
        
        // Second position
        assert_eq!(entries[1].move_count, 2);
        assert_eq!(entries[1].moves.len(), 2);
        assert_eq!(entries[1].moves[0].move_notation, "3c3d");
        assert_eq!(entries[1].moves[1].move_notation, "8c8d");
        
        // Third position
        assert_eq!(entries[2].move_count, 3);
        assert_eq!(entries[2].moves.len(), 1);
        assert_eq!(entries[2].moves[0].move_notation, "2g2f");
    }

    #[test]
    fn test_move_line_without_position() {
        let mut parser = SfenParser::new();
        
        // Try to parse a move without setting up a position first
        let result = parser.parse_line("7g7f none 50 10 100000");
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_consecutive_sfen_lines() {
        let mut parser = SfenParser::new();
        let mut entries = Vec::new();

        // First position
        let result1 = parser.parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1");
        assert!(result1.unwrap().is_none());

        // Second position immediately - should return first position
        let result2 = parser.parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2");
        let entry = result2.unwrap();
        assert!(entry.is_some());
        entries.push(entry.unwrap());

        // Third position - should return second position
        let result3 = parser.parse_line("sfen lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3");
        let entry = result3.unwrap();
        assert!(entry.is_some());
        entries.push(entry.unwrap());

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].moves.len(), 0); // First position had no moves
        assert_eq!(entries[1].moves.len(), 0); // Second position had no moves
    }

    #[test]
    fn test_parse_multiline_sfen_string() {
        // Test parsing a multi-line string as would come from a file
        let multiline_sfen = r#"#YANEURAOU-DB2016 1.00
sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1
7g7f none 50 10 100000
2g2f none 45 10 95000
6g6f none 40 9 90000
sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2
3c3d none -45 10 98000
8c8d none -40 9 93000
sfen lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3
2g2f none 55 11 110000
8h2b+ none 60 12 120000"#;

        let mut parser = SfenParser::new();
        let mut entries = Vec::new();

        // Parse all lines
        for line in multiline_sfen.lines() {
            if let Ok(Some(entry)) = parser.parse_line(line) {
                entries.push(entry);
            }
        }

        // Get the last entry
        if let Ok(Some(entry)) = parser.parse_line("") {
            entries.push(entry);
        }

        // Verify results
        assert_eq!(entries.len(), 3);

        // First position: 3 moves
        assert_eq!(entries[0].position, "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL");
        assert_eq!(entries[0].turn, 'b');
        assert_eq!(entries[0].move_count, 1);
        assert_eq!(entries[0].moves.len(), 3);
        assert_eq!(entries[0].moves[0].move_notation, "7g7f");
        assert_eq!(entries[0].moves[1].move_notation, "2g2f");
        assert_eq!(entries[0].moves[2].move_notation, "6g6f");

        // Second position: 2 moves
        assert_eq!(entries[1].position, "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL");
        assert_eq!(entries[1].turn, 'w');
        assert_eq!(entries[1].move_count, 2);
        assert_eq!(entries[1].moves.len(), 2);
        assert_eq!(entries[1].moves[0].move_notation, "3c3d");
        assert_eq!(entries[1].moves[1].move_notation, "8c8d");

        // Third position: 2 moves
        assert_eq!(entries[2].position, "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL");
        assert_eq!(entries[2].turn, 'b');
        assert_eq!(entries[2].move_count, 3);
        assert_eq!(entries[2].moves.len(), 2);
        assert_eq!(entries[2].moves[0].move_notation, "2g2f");
        assert_eq!(entries[2].moves[1].move_notation, "8h2b+");
    }

    #[test]
    fn test_parse_real_yaneuraou_format() {
        // Test with actual YaneuraOu format data
        let real_data = r#"#YANEURAOU-DB2016 1.00
sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0
8i7g none 194 3 0
3d3c+ none -203 0 0
7h7g none -325 0 0
8a5d none -854 0 0
sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1pprP4/P1N2P2N/1PG1G4/L1K5R b NL2P2slp 0
8a5d none 194 1 0
5h6g none -188 0 0
3d3c+ none -194 0 0
P*6g none -208 0 0"#;

        let mut parser = SfenParser::new();
        let mut entries = Vec::new();

        for line in real_data.lines() {
            if let Ok(Some(entry)) = parser.parse_line(line) {
                entries.push(entry);
            }
        }

        // Get the last entry
        if let Ok(Some(entry)) = parser.parse_line("") {
            entries.push(entry);
        }

        assert_eq!(entries.len(), 2);

        // First complex position
        assert_eq!(entries[0].position, "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R");
        assert_eq!(entries[0].hand, "NLP2sl2p");
        assert_eq!(entries[0].move_count, 0);
        assert_eq!(entries[0].moves.len(), 4);
        assert_eq!(entries[0].moves[0].evaluation, 194);
        assert_eq!(entries[0].moves[1].evaluation, -203);

        // Second complex position
        assert_eq!(entries[1].position, "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1pprP4/P1N2P2N/1PG1G4/L1K5R");
        assert_eq!(entries[1].hand, "NL2P2slp");
        assert_eq!(entries[1].moves.len(), 4);
    }
}
