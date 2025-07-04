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

        // Start new entry without completing first
        let result = parser.parse_line(
            "sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0",
        );
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Should start new position, no completed entry yet
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
}
