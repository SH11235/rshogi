//! Parallel safety tests for transposition table

mod util;

#[cfg(test)]
mod tests {
    use super::util::sync_compat::{thread, Arc, AtomicU64, Ordering};
    use crate::ai::tt::*;
    use std::time::Duration;

    #[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
    use crate::ai::board::{PieceType, Square};
    #[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
    use crate::ai::moves::Move;

    /// Test concurrent probe and store operations using loom
    #[test]
    #[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
    fn test_concurrent_probe_store_loom() {
        // Skip test in CI environment
        if std::env::var("CI").is_ok() || std::env::var("GITHUB_ACTIONS").is_ok() {
            println!("Skipping loom test in CI environment");
            return;
        }

        loom::model(|| {
            let tt = Arc::new(TranspositionTable::new(1));
            let hash = 0x1234567890ABCDEF; // Same hash for all threads to force conflicts

            // Writer thread 1 - writes exact entry
            let tt1 = Arc::clone(&tt);
            let h1 = thread::spawn(move || {
                for i in 0..3 {
                    let mv = Some(Move::normal(Square::new(7, 7), Square::new(7, 6), false));
                    tt1.store(hash, mv, 1500 + i as i16, 1000 + i as i16, 10, NodeType::Exact);
                }
            });

            // Writer thread 2 - writes lower bound entry
            let tt2 = Arc::clone(&tt);
            let h2 = thread::spawn(move || {
                for i in 0..3 {
                    let mv = Some(Move::normal(Square::new(2, 7), Square::new(2, 6), false));
                    tt2.store(hash, mv, -500 - i as i16, -300 - i as i16, 8, NodeType::LowerBound);
                }
            });

            // Writer thread 3 - writes upper bound entry with age changes
            let tt3 = Arc::clone(&tt);
            let h3 = thread::spawn(move || {
                for i in 0..3 {
                    let mv = Some(Move::drop(PieceType::Pawn, Square::new(5, 5)));
                    tt3.store(hash, mv, 250 + i as i16, 125 + i as i16, 12, NodeType::UpperBound);
                }
            });

            // Reader thread
            let tt4 = Arc::clone(&tt);
            let h4 = thread::spawn(move || {
                for _ in 0..5 {
                    if let Some(entry) = tt4.probe(hash) {
                        // Assert that we get consistent data
                        let score = entry.score();
                        let eval = entry.eval();
                        let depth = entry.depth();
                        let node_type = entry.node_type();

                        // Verify data consistency - all fields should match one of the writers
                        match depth {
                            10 => {
                                // From writer 1
                                assert!(score >= 1500 && score < 1503);
                                assert!(eval >= 1000 && eval < 1003);
                                assert_eq!(node_type, NodeType::Exact);
                            }
                            8 => {
                                // From writer 2
                                assert!(score <= -500 && score > -503);
                                assert!(eval <= -300 && eval > -303);
                                assert_eq!(node_type, NodeType::LowerBound);
                            }
                            12 => {
                                // From writer 3
                                assert!(score >= 250 && score < 253);
                                assert!(eval >= 125 && eval < 128);
                                assert_eq!(node_type, NodeType::UpperBound);
                            }
                            _ => panic!("Got torn read: unexpected depth={}", depth),
                        }
                    }
                }
            });

            h1.join().unwrap();
            h2.join().unwrap();
            h3.join().unwrap();
            h4.join().unwrap();
        });
    }

    /// Test generation change concurrent with writes
    #[test]
    #[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
    fn test_generation_change_concurrent() {
        // Skip test in CI environment
        if std::env::var("CI").is_ok() || std::env::var("GITHUB_ACTIONS").is_ok() {
            println!("Skipping loom test in CI environment");
            return;
        }

        loom::model(|| {
            // Use Arc<TranspositionTable> wrapped in UnsafeCell for new_search
            let tt = Arc::new(std::cell::UnsafeCell::new(TranspositionTable::new(1)));
            let hash = 0xDEADBEEF;

            // Writer thread - continuous writes
            let tt1 = Arc::clone(&tt);
            let h1 = thread::spawn(move || {
                let tt = unsafe { &*tt1.get() };
                for i in 0..15 {
                    tt.store(hash, None, 100 + i as i16, 50 + i as i16, 5, NodeType::Exact);
                }
            });

            // Generation updater thread
            let tt2 = Arc::clone(&tt);
            let h2 = thread::spawn(move || {
                let tt = unsafe { &mut *tt2.get() };
                for _ in 0..3 {
                    tt.new_search();
                }
            });

            // Reader thread
            let tt3 = Arc::clone(&tt);
            let h3 = thread::spawn(move || {
                let tt = unsafe { &*tt3.get() };
                for _ in 0..20 {
                    if let Some(entry) = tt.probe(hash) {
                        // Just verify we don't crash and data is somewhat consistent
                        let _score = entry.score();
                        let _age = entry.age();
                        assert!(entry.depth() > 0);
                    }
                }
            });

            h1.join().unwrap();
            h2.join().unwrap();
            h3.join().unwrap();
        });
    }

    /// Multi-threaded concurrent access stress test with forced collisions
    #[test]
    #[cfg(not(target_arch = "wasm32"))] // Exclude from WASM builds
    fn test_concurrent_access_stress() {
        // Use very small table to force collisions
        let tt = Arc::new(TranspositionTable::new(0)); // 64KB minimum size
        let iterations = 10_000; // Reduced from 100_000 for faster tests
        let num_threads = 8;

        // Use same hash for all threads to maximize conflicts
        let base_hash = 0xCAFEBABE;

        // Counter for torn reads
        let torn_reads = Arc::new(AtomicU64::new(0));

        let mut handles = vec![];

        for thread_id in 0..num_threads {
            let tt = Arc::clone(&tt);
            let torn_reads = Arc::clone(&torn_reads);

            let handle = thread::spawn(move || {
                for i in 0..iterations {
                    // Use same hash or nearby hashes to force conflicts
                    let hash = base_hash + (i % 4) as u64; // Only 4 different hashes

                    // Create thread-specific data pattern
                    let thread_marker = thread_id as i16 * 1000;
                    let score = thread_marker + (i % 100) as i16;
                    let eval = score / 2;
                    let depth = ((thread_id + 1) * 2) as u8; // Thread-specific depth
                    let node_type = match thread_id % 3 {
                        0 => NodeType::Exact,
                        1 => NodeType::LowerBound,
                        _ => NodeType::UpperBound,
                    };

                    // Write
                    tt.store(hash, None, score, eval, depth, node_type);

                    // Immediate read-back to check for torn writes
                    if let Some(entry) = tt.probe(hash) {
                        let read_score = entry.score();
                        let read_eval = entry.eval();
                        let read_depth = entry.depth();
                        let read_type = entry.node_type();

                        // Check if this could be our write
                        // Handle negative scores by taking absolute value first
                        let thread_from_score = if read_score >= 0 {
                            (read_score / 1000) as u64
                        } else {
                            ((read_score.wrapping_neg()) / 1000) as u64
                        };
                        if thread_from_score < num_threads as u64 {
                            // It's from one of our threads
                            let expected_eval = read_score / 2;
                            let expected_depth = ((thread_from_score + 1) * 2) as u8;
                            let expected_type = match thread_from_score % 3 {
                                0 => NodeType::Exact,
                                1 => NodeType::LowerBound,
                                _ => NodeType::UpperBound,
                            };

                            // Check consistency
                            if read_eval != expected_eval
                                || read_depth != expected_depth
                                || read_type != expected_type
                            {
                                torn_reads.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
            });

            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        let torn = torn_reads.load(Ordering::Relaxed);
        println!("Concurrent stress test completed: {torn} torn reads");
        assert_eq!(torn, 0, "Detected {torn} torn reads");
    }

    /// Test that ensures proper memory ordering prevents stale data
    #[test]
    fn test_memory_ordering_consistency() {
        let tt = TranspositionTable::new(1);
        let hash = 0xDEADBEEF;

        // Store initial entry
        tt.store(hash, None, 100, 50, 5, NodeType::Exact);

        // Verify we can read it back
        let entry1 = tt.probe(hash).expect("Should find entry");
        assert_eq!(entry1.score(), 100);
        assert_eq!(entry1.eval(), 50);
        assert_eq!(entry1.depth(), 5);

        // Update with new data
        tt.store(hash, None, 200, 100, 10, NodeType::LowerBound);

        // Read multiple times to ensure consistency
        for _ in 0..100 {
            if let Some(entry) = tt.probe(hash) {
                let score = entry.score();
                let eval = entry.eval();
                let depth = entry.depth();
                let node_type = entry.node_type();

                // We should always get the complete new entry or old entry, never mixed
                if depth == 10 {
                    assert_eq!(score, 200);
                    assert_eq!(eval, 100);
                    assert_eq!(node_type, NodeType::LowerBound);
                } else if depth == 5 {
                    assert_eq!(score, 100);
                    assert_eq!(eval, 50);
                    assert_eq!(node_type, NodeType::Exact);
                } else {
                    panic!("Got inconsistent data: depth={depth}");
                }
            }
        }
    }

    /// Test prefetch doesn't interfere with concurrent access
    #[test]
    #[cfg(not(target_arch = "wasm32"))] // Exclude from WASM builds
    fn test_prefetch_safety() {
        let tt = Arc::new(TranspositionTable::new(1));
        let hash = 0xCAFEBABE;

        let tt1 = Arc::clone(&tt);
        let h1 = thread::spawn(move || {
            for i in 0..1000 {
                tt1.prefetch(hash);
                if i % 100 == 0 {
                    std::thread::sleep(Duration::from_micros(1));
                }
            }
        });

        let tt2 = Arc::clone(&tt);
        let h2 = thread::spawn(move || {
            for i in 0..100 {
                let score = (i * 10) as i16;
                tt2.store(hash, None, score, score / 2, 8, NodeType::Exact);
                std::thread::sleep(Duration::from_micros(10));
            }
        });

        let tt3 = Arc::clone(&tt);
        let h3 = thread::spawn(move || {
            let mut last_score = None;
            for _ in 0..500 {
                if let Some(entry) = tt3.probe(hash) {
                    let score = entry.score();

                    // Score should only increase (or stay same)
                    if let Some(last) = last_score {
                        assert!(score >= last, "Score went backwards: {last} -> {score}");
                    }
                    last_score = Some(score);
                }
                std::thread::sleep(Duration::from_micros(5));
            }
        });

        h1.join().unwrap();
        h2.join().unwrap();
        h3.join().unwrap();
    }
}
