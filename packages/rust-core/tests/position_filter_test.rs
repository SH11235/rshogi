#[cfg(test)]
mod position_filter_tests {
    use shogi_core::opening_book::*;

    fn create_test_entry(move_count: u32, moves: Vec<(i32, u32)>) -> RawSfenEntry {
        RawSfenEntry {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count,
            moves: moves.into_iter().map(|(eval, depth)| RawMove {
                move_notation: "7g7f".to_string(),
                move_type: "none".to_string(),
                evaluation: eval,
                depth,
                nodes: 1000,
            }).collect(),
        }
    }

    #[test]
    fn test_default_filter() {
        let filter = PositionFilter::default();
        
        assert_eq!(filter.max_moves, 50);
        assert_eq!(filter.min_depth, 0);
        assert_eq!(filter.min_evaluation, -1000);
        assert_eq!(filter.max_evaluation, 1000);
    }

    #[test]
    fn test_filter_by_move_count() {
        let filter = PositionFilter {
            max_moves: 20,
            ..Default::default()
        };
        
        let early_game = create_test_entry(10, vec![(50, 1)]);
        let mid_game = create_test_entry(25, vec![(50, 1)]);
        let late_game = create_test_entry(100, vec![(50, 1)]);
        
        assert!(filter.should_include(&early_game));
        assert!(!filter.should_include(&mid_game));
        assert!(!filter.should_include(&late_game));
    }

    #[test]
    fn test_filter_empty_moves() {
        let filter = PositionFilter::default();
        
        let entry_with_moves = create_test_entry(10, vec![(50, 1)]);
        let entry_without_moves = create_test_entry(10, vec![]);
        
        assert!(filter.should_include(&entry_with_moves));
        assert!(!filter.should_include(&entry_without_moves));
    }

    #[test]
    fn test_filter_by_depth() {
        let filter = PositionFilter {
            min_depth: 5,
            ..Default::default()
        };
        
        let shallow_entry = create_test_entry(10, vec![(50, 2), (40, 3), (30, 4)]);
        let deep_entry = create_test_entry(10, vec![(50, 2), (40, 3), (30, 6)]);
        let all_deep_entry = create_test_entry(10, vec![(50, 5), (40, 7), (30, 10)]);
        
        assert!(!filter.should_include(&shallow_entry));
        assert!(filter.should_include(&deep_entry)); // At least one move meets depth
        assert!(filter.should_include(&all_deep_entry));
    }

    #[test]
    fn test_filter_by_evaluation() {
        let filter = PositionFilter {
            min_evaluation: -500,
            max_evaluation: 500,
            ..Default::default()
        };
        
        let balanced = create_test_entry(10, vec![(50, 1), (-30, 1), (100, 1)]);
        let winning = create_test_entry(10, vec![(600, 1), (550, 1), (700, 1)]);
        let losing = create_test_entry(10, vec![(-600, 1), (-550, 1), (-700, 1)]);
        let mixed = create_test_entry(10, vec![(400, 1), (-600, 1), (700, 1)]);
        
        assert!(filter.should_include(&balanced));
        assert!(!filter.should_include(&winning)); // Best move > 500
        assert!(!filter.should_include(&losing));  // Best move < -500
        assert!(!filter.should_include(&mixed));   // Best move (700) is > 500
    }

    #[test]
    fn test_filter_moves() {
        let filter = PositionFilter::default();
        
        // Create entry with many moves
        let mut entry = RawSfenEntry {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 10,
            moves: vec![
                RawMove { move_notation: "7g7f".to_string(), move_type: "none".to_string(), evaluation: 100, depth: 1, nodes: 1000 },
                RawMove { move_notation: "2g2f".to_string(), move_type: "none".to_string(), evaluation: 90, depth: 1, nodes: 1000 },
                RawMove { move_notation: "6g6f".to_string(), move_type: "none".to_string(), evaluation: 80, depth: 1, nodes: 1000 },
                RawMove { move_notation: "5g5f".to_string(), move_type: "none".to_string(), evaluation: 70, depth: 1, nodes: 1000 },
                RawMove { move_notation: "4g4f".to_string(), move_type: "none".to_string(), evaluation: 60, depth: 1, nodes: 1000 },
                RawMove { move_notation: "3g3f".to_string(), move_type: "none".to_string(), evaluation: 50, depth: 1, nodes: 1000 },
                RawMove { move_notation: "1g1f".to_string(), move_type: "none".to_string(), evaluation: 40, depth: 1, nodes: 1000 },
                RawMove { move_notation: "9g9f".to_string(), move_type: "none".to_string(), evaluation: 30, depth: 1, nodes: 1000 },
                RawMove { move_notation: "8g8f".to_string(), move_type: "none".to_string(), evaluation: 20, depth: 1, nodes: 1000 },
                RawMove { move_notation: "P*5f".to_string(), move_type: "none".to_string(), evaluation: 10, depth: 1, nodes: 1000 },
            ],
        };
        
        filter.filter_moves(&mut entry);
        
        // Should keep only top 8 moves
        assert_eq!(entry.moves.len(), 8);
        
        // Should be sorted by evaluation (descending)
        assert_eq!(entry.moves[0].evaluation, 100);
        assert_eq!(entry.moves[1].evaluation, 90);
        assert_eq!(entry.moves[7].evaluation, 30);
        
        // Should not include the lowest evaluated moves
        assert!(!entry.moves.iter().any(|m| m.evaluation == 20));
        assert!(!entry.moves.iter().any(|m| m.evaluation == 10));
    }

    #[test]
    fn test_filter_moves_less_than_eight() {
        let filter = PositionFilter::default();
        
        let mut entry = create_test_entry(10, vec![(100, 1), (90, 1), (80, 1)]);
        let original_len = entry.moves.len();
        
        filter.filter_moves(&mut entry);
        
        // Should keep all moves if less than 8
        assert_eq!(entry.moves.len(), original_len);
        assert_eq!(entry.moves.len(), 3);
    }

    #[test]
    fn test_filter_moves_sorting() {
        let filter = PositionFilter::default();
        
        let mut entry = RawSfenEntry {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 10,
            moves: vec![
                RawMove { move_notation: "3g3f".to_string(), move_type: "none".to_string(), evaluation: 50, depth: 1, nodes: 1000 },
                RawMove { move_notation: "7g7f".to_string(), move_type: "none".to_string(), evaluation: 100, depth: 1, nodes: 1000 },
                RawMove { move_notation: "1g1f".to_string(), move_type: "none".to_string(), evaluation: 40, depth: 1, nodes: 1000 },
                RawMove { move_notation: "2g2f".to_string(), move_type: "none".to_string(), evaluation: 90, depth: 1, nodes: 1000 },
                RawMove { move_notation: "6g6f".to_string(), move_type: "none".to_string(), evaluation: 80, depth: 1, nodes: 1000 },
            ],
        };
        
        filter.filter_moves(&mut entry);
        
        // Should be sorted by evaluation
        assert_eq!(entry.moves[0].move_notation, "7g7f"); // 100
        assert_eq!(entry.moves[1].move_notation, "2g2f"); // 90
        assert_eq!(entry.moves[2].move_notation, "6g6f"); // 80
        assert_eq!(entry.moves[3].move_notation, "3g3f"); // 50
        assert_eq!(entry.moves[4].move_notation, "1g1f"); // 40
    }

    #[test]
    fn test_complex_filtering_scenario() {
        let filter = PositionFilter {
            max_moves: 30,
            min_depth: 3,
            min_evaluation: -200,
            max_evaluation: 200,
        };
        
        // Position that passes all filters
        let good_entry = RawSfenEntry {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 20,
            moves: vec![
                RawMove { move_notation: "7g7f".to_string(), move_type: "none".to_string(), evaluation: 150, depth: 5, nodes: 1000 },
                RawMove { move_notation: "2g2f".to_string(), move_type: "none".to_string(), evaluation: 100, depth: 2, nodes: 1000 },
                RawMove { move_notation: "6g6f".to_string(), move_type: "none".to_string(), evaluation: -50, depth: 1, nodes: 1000 },
            ],
        };
        
        assert!(filter.should_include(&good_entry)); // Has depth >= 3 and best eval in range
        
        // Position that fails depth requirement
        let shallow_entry = RawSfenEntry {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 20,
            moves: vec![
                RawMove { move_notation: "7g7f".to_string(), move_type: "none".to_string(), evaluation: 150, depth: 2, nodes: 1000 },
                RawMove { move_notation: "2g2f".to_string(), move_type: "none".to_string(), evaluation: 100, depth: 1, nodes: 1000 },
            ],
        };
        
        assert!(!filter.should_include(&shallow_entry)); // No move with depth >= 3
    }
}