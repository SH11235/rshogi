//! Integration tests for SfenParser
//! This file contains tests that require file I/O operations

#[cfg(test)]
mod sfen_parser_integration_tests {
    use shogi_core::opening_book::SfenParser;
    use std::fs;
    use std::path::Path;

    #[test]
    fn test_parse_mini_user_book_file() {
        // Test parsing the actual mini_user_book_head99.db file
        let file_path = Path::new("tests/data/mini_user_book_head99.db");

        // Read the file content
        let content =
            fs::read_to_string(file_path).expect("Failed to read mini_user_book_head99.db");

        let mut parser = SfenParser::new();
        let mut entries = Vec::new();
        let mut total_lines = 0;
        let mut position_count = 0;
        let mut move_count = 0;

        // Parse all lines
        for line in content.lines() {
            total_lines += 1;

            if line.starts_with("sfen ") {
                position_count += 1;
            } else if !line.starts_with('#') && !line.trim().is_empty() {
                // This should be a move line
                if line.split_whitespace().count() >= 5 {
                    move_count += 1;
                }
            }

            if let Ok(Some(entry)) = parser.parse_line(line) {
                entries.push(entry);
            }
        }

        // Get the last entry if any
        if let Ok(Some(entry)) = parser.parse_line("") {
            entries.push(entry);
        }

        // Verify that we parsed entries
        assert!(!entries.is_empty(), "Should have parsed at least one entry");

        // Verify basic properties of parsed entries
        for entry in &entries {
            // Each entry should have a valid position
            assert!(!entry.position.is_empty());

            // Turn should be either 'b' or 'w'
            assert!(entry.turn == 'b' || entry.turn == 'w');

            // Move count is u32, always non-negative

            // If there are moves, verify their structure
            for mv in &entry.moves {
                assert!(!mv.move_notation.is_empty());
                // Move notation should not be "none"
                assert_ne!(mv.move_notation, "none");
            }
        }

        println!("Parsed {} entries from {} lines", entries.len(), total_lines);
        println!("Found {position_count} positions and {move_count} moves");

        // Verify we parsed the exact number of entries
        // The file has 18 positions with multiple moves each
        assert_eq!(
            entries.len(),
            18,
            "Expected exactly 18 entries from mini_user_book_head99.db, but got {}",
            entries.len()
        );
    }
}
