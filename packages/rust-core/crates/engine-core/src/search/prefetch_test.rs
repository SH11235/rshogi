//! Tests for transposition table prefetch functionality

#[cfg(test)]
mod tests {
    use crate::search::adaptive_prefetcher::AdaptivePrefetcher;
    use crate::search::tt::{NodeType, TranspositionTable};
    use crate::shogi::board::Position;
    use crate::shogi::{Move, Square};

    #[test]
    fn test_basic_prefetch() {
        let tt = TranspositionTable::new(16); // 16MB table
        let pos = Position::startpos();
        let hash = pos.zobrist_hash();

        // Test different cache levels
        tt.prefetch_l1(hash);
        tt.prefetch_l2(hash);
        tt.prefetch_l3(hash);

        // Store an entry
        let mv = Move::make_normal(Square::new(7, 7), Square::new(7, 6));

        tt.store(hash, Some(mv), 100, 50, 10, NodeType::Exact);

        // Prefetch and probe should work
        tt.prefetch_l1(hash);
        let entry = tt.probe(hash);
        assert!(entry.is_some());

        if let Some(entry) = entry {
            assert_eq!(entry.score(), 100);
            assert_eq!(entry.depth(), 10);
        }
    }

    #[test]
    fn test_prefetch_with_hint() {
        let tt = TranspositionTable::new(16);
        let pos = Position::startpos();

        // Test all hint levels
        for hint in 0..=3 {
            tt.prefetch(pos.zobrist_hash(), hint);
        }

        // Store and retrieve
        tt.store(pos.zobrist_hash(), None, 200, 100, 15, NodeType::LowerBound);

        // Prefetch with specific hint
        tt.prefetch(pos.zobrist_hash(), 1); // L2 cache

        let entry = tt.probe(pos.zobrist_hash());
        assert!(entry.is_some());
    }

    #[test]
    fn test_adaptive_prefetcher_integration() {
        let prefetcher = AdaptivePrefetcher::new();
        let tt = TranspositionTable::new(16);

        // Simulate prefetch with adaptive distance
        let pos = Position::startpos();
        let distance = prefetcher.calculate_distance(1000);

        // Prefetch multiple entries based on distance
        for i in 0..distance {
            let hash = pos.zobrist_hash() ^ (i as u64);
            tt.prefetch_l1(hash);
        }

        // Record hits and misses
        for i in 0..distance {
            let hash = pos.zobrist_hash() ^ (i as u64);
            if tt.probe(hash).is_some() {
                prefetcher.record_hit();
            } else {
                prefetcher.record_miss();
            }
        }

        // Check statistics
        let stats = prefetcher.stats();
        assert_eq!(stats.total(), distance as u64);
    }

    #[test]
    fn test_prefetch_performance() {
        use std::time::Instant;

        let tt = TranspositionTable::new(128); // Larger table for performance test
        let pos = Position::startpos();

        // Generate test hashes
        let test_hashes: Vec<u64> = (0..10000).map(|i| pos.zobrist_hash() ^ (i as u64)).collect();

        // Store entries
        for &hash in &test_hashes[0..5000] {
            tt.store(hash, None, 100, 50, 10, NodeType::Exact);
        }

        // Test without prefetch
        let start = Instant::now();
        let mut hits_without = 0;
        for &hash in &test_hashes {
            if tt.probe(hash).is_some() {
                hits_without += 1;
            }
        }
        let time_without = start.elapsed();

        // Test with prefetch
        let start = Instant::now();
        let mut hits_with = 0;
        for (i, &hash) in test_hashes.iter().enumerate() {
            // Prefetch next few entries
            for j in 1..=3 {
                if i + j < test_hashes.len() {
                    tt.prefetch_l1(test_hashes[i + j]);
                }
            }

            if tt.probe(hash).is_some() {
                hits_with += 1;
            }
        }
        let time_with = start.elapsed();

        // Both should find the same number of hits
        assert_eq!(hits_without, hits_with);
        assert_eq!(hits_without, 5000);

        // Log performance difference (prefetch might not always be faster in tests)
        println!("Without prefetch: {time_without:?}, With prefetch: {time_with:?}");
    }

    #[test]
    fn test_cache_level_selection() {
        let tt = TranspositionTable::new(16);
        let pos = Position::startpos();

        // Test that different cache levels work
        let depths = [
            (0, 0),  // Shallow: L1
            (5, 0),  // Shallow: L1
            (10, 1), // Medium: L2
            (15, 1), // Medium: L2
            (20, 2), // Deep: L3
            (25, 2), // Deep: L3
        ];

        for (depth, expected_hint) in depths {
            // Calculate appropriate hint based on depth
            let hint = if depth < 8 {
                0 // L1
            } else if depth < 16 {
                1 // L2
            } else {
                2 // L3
            };

            assert_eq!(hint, expected_hint);

            // Prefetch with calculated hint
            tt.prefetch(pos.zobrist_hash() ^ depth, hint);
        }
    }
}
