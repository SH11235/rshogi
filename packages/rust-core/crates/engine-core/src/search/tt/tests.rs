//! Test modules for transposition table

use super::constants::{AGE_MASK, GENERATION_CYCLE};
use super::*;
use crate::shogi::Position;
use std::sync::Arc;
use std::thread;

// Ensure proper alignment
#[cfg(test)]
mod alignment_tests {
    use super::*;

    #[test]
    fn test_entry_alignment() {
        assert_eq!(std::mem::size_of::<TTEntry>(), 16);
        assert_eq!(std::mem::align_of::<TTEntry>(), 16);
    }

    #[test]
    fn test_bucket_alignment() {
        assert_eq!(std::mem::size_of::<bucket::TTBucket>(), 64);
        assert_eq!(std::mem::align_of::<bucket::TTBucket>(), 64);
    }
}

// Performance tests for CAS retry improvements
#[cfg(test)]
mod performance_tests {
    use super::*;

    #[test]
    fn test_high_contention_cas_retry() {
        let tt = Arc::new(TranspositionTable::new(1)); // 1MB table
        let num_threads = 4;
        let updates_per_thread = 100; // Reduced for test speed

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let tt_clone = Arc::clone(&tt);
                thread::spawn(move || {
                    for i in 0..updates_per_thread {
                        // Generate non-zero hash key
                        let key = ((thread_id * updates_per_thread + i + 1) as u64)
                            | 0x8000_0000_0000_0000;
                        // This tests the new CAS retry logic under contention
                        tt_clone.store(
                            key,
                            None,
                            200 + (i % 10) as i16,
                            100,
                            (i % 20) as u8,
                            entry::NodeType::Exact,
                        );
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify no panics occurred - that's the main test for correctness
        // under high contention with the new CAS retry logic
        println!("High contention CAS retry test completed successfully");
    }
}

#[cfg(test)]
mod tt_tests {
    use super::*;
    use crate::util::sync_compat::Ordering;

    #[test]
    fn test_bitmap_operations() {
        let tt = TranspositionTable::new(1); // 1MB table

        // Initially all buckets should be unoccupied
        assert!(!tt.is_bucket_occupied(0));
        assert!(!tt.is_bucket_occupied(100));

        // Mark some buckets as occupied
        tt.mark_bucket_occupied(0);
        tt.mark_bucket_occupied(100);
        tt.mark_bucket_occupied(255);

        // Check they are marked
        assert!(tt.is_bucket_occupied(0));
        assert!(tt.is_bucket_occupied(100));
        assert!(tt.is_bucket_occupied(255));

        // Check others remain unoccupied
        assert!(!tt.is_bucket_occupied(1));
        assert!(!tt.is_bucket_occupied(99));
        assert!(!tt.is_bucket_occupied(256));
    }

    #[test]
    fn test_hashfull_estimate() {
        let tt = TranspositionTable::new(1); // 1MB table

        // Initially estimate should be 0
        assert_eq!(tt.hashfull_estimate(), 0);

        // Mark some buckets and update estimate
        for i in 0..64 {
            tt.mark_bucket_occupied(i);
        }
        tt.update_hashfull_estimate();

        // Since we marked all 64 sampled buckets, estimate should be 1000
        // But due to EMA, it won't jump immediately
        let estimate = tt.hashfull_estimate();
        assert!(estimate > 0);

        // Multiple updates should converge
        for _ in 0..10 {
            tt.update_hashfull_estimate();
        }
        // After convergence, should be close to 1000 if all sampled buckets are occupied
        // But exact value depends on sampling
    }

    #[test]
    fn test_store_updates_bitmap() {
        let tt = TranspositionTable::new(1); // 1MB table
        let position = Position::startpos();
        let hash = position.hash;
        let bucket_idx = tt.bucket_index(hash);

        // Initially bucket should be unoccupied
        assert!(!tt.is_bucket_occupied(bucket_idx));

        // Store an entry
        tt.store(hash, None, 100, 50, 5, NodeType::Exact);

        // Bucket should now be occupied
        assert!(tt.is_bucket_occupied(bucket_idx));
    }

    #[test]
    fn test_clear_resets_bitmap() {
        let mut tt = TranspositionTable::new(1); // 1MB table

        // Mark some buckets and set estimate
        for i in 0..100 {
            tt.mark_bucket_occupied(i);
        }
        tt.hashfull_estimate.store(500, Ordering::Relaxed);
        tt.node_counter.store(1000, Ordering::Relaxed);

        // Clear the table
        tt.clear();

        // All buckets should be unoccupied
        for i in 0..100 {
            assert!(!tt.is_bucket_occupied(i));
        }

        // Estimates and counters should be reset
        assert_eq!(tt.hashfull_estimate(), 0);
        assert_eq!(tt.node_counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[cfg(all(feature = "hashfull_filter", feature = "tt_metrics"))]
    fn test_hashfull_filtering() {
        let mut tt = TranspositionTable::new(1); // 1MB table
        tt.enable_metrics();

        // Set hashfull estimate to 750 (75%)
        tt.hashfull_estimate.store(750, Ordering::Relaxed);

        // Try to store a shallow entry (depth=1) - should be filtered at 75%
        let position = Position::startpos();
        tt.store(
            position.hash,
            None,
            100,
            50,
            1, // very shallow depth - will be filtered when threshold is 2
            NodeType::LowerBound,
        );

        // Should be filtered (750 is in 600-800 range, so depth < 2 is filtered)
        let metrics = tt.metrics.as_ref().unwrap();
        assert_eq!(metrics.hashfull_filtered.load(Ordering::Relaxed), 1);

        // Set hashfull estimate to 900 (90%)
        tt.hashfull_estimate.store(900, Ordering::Relaxed);

        // Try to store a non-exact entry
        tt.store(
            position.hash + 1,
            None,
            100,
            50,
            10,                   // deep enough
            NodeType::LowerBound, // not exact
        );

        // Should be filtered
        assert_eq!(metrics.hashfull_filtered.load(Ordering::Relaxed), 2);

        // Try to store an exact entry
        tt.store(
            position.hash + 2,
            None,
            100,
            50,
            10,
            NodeType::Exact, // exact node
        );

        // Should NOT be filtered
        assert_eq!(metrics.hashfull_filtered.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_abdada_flag_operations() {
        let tt = TranspositionTable::new(1); // 1MB table
        let hash = 0x1234567890ABCDEF;

        // Store an entry
        tt.store(hash, None, 100, 50, 5, NodeType::Exact);

        // Initially, exact cut flag should not be set
        let entry = tt.probe(hash).unwrap();
        assert!(!entry.has_abdada_cut());

        // Set the exact cut flag
        assert!(tt.set_exact_cut(hash));

        // Now the flag should be set
        let entry = tt.probe(hash).unwrap();
        assert!(entry.has_abdada_cut());

        // Clear the flag
        assert!(tt.clear_exact_cut(hash));

        // Flag should be cleared
        let entry = tt.probe(hash).unwrap();
        assert!(!entry.has_abdada_cut());

        // Test non-existent entry
        let non_existent_hash = 0xFEDCBA0987654321;
        assert!(tt.probe(non_existent_hash).is_none());
        assert!(!tt.set_exact_cut(non_existent_hash));
        assert!(!tt.clear_exact_cut(non_existent_hash));
    }

    #[test]
    fn test_abdada_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let tt = Arc::new(TranspositionTable::new(4)); // 4MB table
        let hash = 0xDEADBEEF12345678;

        // Store an entry
        tt.store(hash, None, 200, 100, 10, NodeType::LowerBound);

        // Create multiple threads trying to set the flag concurrently
        let mut handles = vec![];
        for _ in 0..8 {
            let tt_clone = Arc::clone(&tt);
            let handle = thread::spawn(move || {
                // Each thread tries to set the flag multiple times
                let mut success_count = 0;
                for _ in 0..100 {
                    if tt_clone.set_exact_cut(hash) {
                        success_count += 1;
                    }
                }
                success_count
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All threads should succeed in setting the flag
        for count in results {
            assert!(count > 0);
        }

        // The flag should still be set
        let entry = tt.probe(hash).unwrap();
        assert!(entry.has_abdada_cut());
    }

    #[test]
    fn test_abdada_flag_persistence() {
        let tt = TranspositionTable::new(1); // 1MB table
        let hash = 0xCAFEBABE87654321;

        // Store entry with some values
        tt.store(hash, None, 150, 75, 8, NodeType::Exact);

        // Set the flag
        assert!(tt.set_exact_cut(hash));

        // Probe the entry and verify other fields are intact
        let entry = tt.probe(hash).unwrap();
        assert_eq!(entry.score(), 150);
        assert_eq!(entry.eval(), 75);
        assert_eq!(entry.depth(), 8);
        assert_eq!(entry.node_type(), NodeType::Exact);

        // Flag should still be set
        assert!(entry.has_abdada_cut());
    }

    #[test]
    fn test_gc_trigger() {
        let tt = TranspositionTable::new(1); // 1MB table

        // Initially GC should not be needed
        assert!(!tt.should_trigger_gc());

        // Simulate high hashfull scenario
        // First set a high hashfull estimate
        tt.hashfull_estimate.store(990, Ordering::Relaxed);

        // Directly trigger GC (simulating what store_entry would do)
        tt.need_gc.store(true, Ordering::Relaxed);

        assert!(tt.should_trigger_gc());

        // Test gradual trigger
        tt.need_gc.store(false, Ordering::Relaxed);
        tt.hashfull_estimate.store(950, Ordering::Relaxed);

        // Simulate multiple high hashfull updates
        for _ in 0..11 {
            tt.high_hashfull_counter.fetch_add(1, Ordering::Relaxed);
        }

        // Check if counter would trigger GC
        if tt.high_hashfull_counter.load(Ordering::Relaxed) >= 10 {
            tt.need_gc.store(true, Ordering::Relaxed);
        }

        assert!(tt.should_trigger_gc());
    }

    #[cfg(feature = "tt_metrics")]
    #[test]
    fn test_cas_key_match_optimization() {
        // Test Phase 5 optimization - ensure data updates are properly synchronized
        use std::sync::Arc;
        use std::thread;

        let mut tt = TranspositionTable::new(1); // 1MB table
        tt.enable_metrics();
        let tt = Arc::new(tt);

        let hash = 0x123456789ABCDEF0;
        let initial_depth = 5;
        let updated_depth = 10;

        // Initial store
        tt.store(hash, None, 100, 50, initial_depth, NodeType::Exact);

        // Spawn multiple threads trying to update the same position
        let mut handles = vec![];
        let num_threads = 4;

        for i in 0..num_threads {
            let tt_clone = Arc::clone(&tt);
            let handle = thread::spawn(move || {
                // Each thread tries to update with a different depth
                let thread_depth = updated_depth + i as u8;
                tt_clone.store(hash, None, 200, 60, thread_depth, NodeType::Exact);

                // Immediately probe to check if the update is visible
                tt_clone.probe(hash)
            });
            handles.push(handle);
        }

        // Collect results
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All threads should see a valid entry
        for result in &results {
            assert!(result.is_some(), "Entry should be found");
            if let Some(entry) = result {
                assert!(entry.depth() >= initial_depth, "Depth should not decrease");
                assert!(
                    entry.depth() <= updated_depth + num_threads as u8,
                    "Depth should be within expected range"
                );
            }
        }

        // Check metrics
        if let Some(metrics) = &tt.metrics {
            let cas_key_match = metrics.cas_key_match.load(Ordering::Relaxed);
            // We expect at least some key matches in high contention scenario
            log::debug!("CAS key matches in test: {cas_key_match}");
        }
    }

    #[test]
    fn test_incremental_gc() {
        let mut tt = TranspositionTable::new(1); // 1MB table
        let position = Position::startpos();

        // Fill table with entries of different ages
        for i in 0..1000 {
            let hash = position.hash + i;
            tt.store_with_params(TTEntryParams {
                key: hash,
                mv: None,
                score: 100,
                eval: 50,
                depth: 5,
                node_type: NodeType::Exact,
                age: (i % 8) as u8, // Various ages
                is_pv: false,
                ..Default::default()
            });
        }

        // Advance age to make some entries old
        tt.age = 6;

        // Trigger GC
        tt.need_gc.store(true, Ordering::Relaxed);

        // Run incremental GC
        let mut complete = false;
        let mut iterations = 0;
        while !complete && iterations < 1000 {
            complete = tt.perform_incremental_gc(256);
            iterations += 1;
        }

        assert!(complete);
        assert!(!tt.should_trigger_gc());

        // Verify some entries were cleared
        #[cfg(feature = "tt_metrics")]
        {
            let cleared = tt.gc_entries_cleared();
            assert!(cleared > 0);
        }
    }

    #[test]
    fn test_age_distance_calculation() {
        // Test various age combinations
        let current_age = 5;
        let calculate_age_distance = |age: u8| -> u8 {
            ((GENERATION_CYCLE + current_age as u16 - age as u16) & (AGE_MASK as u16)) as u8
        };

        assert_eq!(calculate_age_distance(5), 0); // Same age
        assert_eq!(calculate_age_distance(4), 1); // 1 generation old
        assert_eq!(calculate_age_distance(3), 2); // 2 generations old
        assert_eq!(calculate_age_distance(1), 4); // 4 generations old

        // Test wraparound
        let current_age = 1;
        let calculate_age_distance = |age: u8| -> u8 {
            ((GENERATION_CYCLE + current_age as u16 - age as u16) & (AGE_MASK as u16)) as u8
        };
        assert_eq!(calculate_age_distance(7), 2); // Wrapped around
        assert_eq!(calculate_age_distance(6), 3); // Wrapped around
    }

    // Test probe functionality (uses SIMD when available)
    #[test]
    fn test_tt_probe_functionality() {
        let bucket = bucket::TTBucket::new();
        let test_entry = TTEntry::new(0x1234567890ABCDEF, None, 100, -50, 10, NodeType::Exact, 0);

        // Store an entry
        bucket.store(test_entry, 0);

        // Probe should find the entry
        let result = bucket.probe(test_entry.key);
        assert!(result.is_some());

        let found_entry = result.unwrap();
        assert_eq!(found_entry.key(), test_entry.key());
        assert_eq!(found_entry.score(), 100);
        assert_eq!(found_entry.eval(), -50);
        assert_eq!(found_entry.depth(), 10);
        assert_eq!(found_entry.node_type(), NodeType::Exact);

        // Non-existent key should return None
        let missing = bucket.probe(0xDEADBEEF);
        assert!(missing.is_none());
    }

    // Test SIMD store priority calculation
    #[test]
    fn test_tt_store_simd_priority() {
        let bucket = bucket::TTBucket::new();

        // Fill bucket with test entries with unique keys
        for i in 0..BUCKET_SIZE {
            // Use high bits to ensure unique keys after shift
            let key = (0x1000000000000000_u64 * (i as u64 + 1)) | 0xFFFF;
            let entry = TTEntry::new(
                key,
                None,
                50 + i as i16 * 10,
                -20 + i as i16 * 5,
                5 + i as u8,
                if i % 2 == 0 {
                    NodeType::Exact
                } else {
                    NodeType::LowerBound
                },
                i as u8,
            );
            bucket.store(entry, 0);
        }

        // Test both SIMD and scalar find worst entry
        let current_age = 4;

        // Since the methods are private, we can't test them directly
        // Instead, test that storing a new entry works correctly
        let new_entry = TTEntry::new(
            0xDEADBEEF,
            None,
            1000, // High score
            500,
            20, // High depth
            NodeType::Exact,
            current_age,
        );

        bucket.store(new_entry, current_age);

        // Verify the new entry was stored
        let result = bucket.probe(0xDEADBEEF);
        assert!(result.is_some());
        let stored = result.unwrap();
        assert_eq!(stored.score(), 1000);
        assert_eq!(stored.depth(), 20);
    }

    // Test depth filter functionality
    #[test]
    fn test_tt_depth_filter() {
        let bucket = bucket::TTBucket::new();

        // Store an entry with depth 10
        let key = 0x1234567890ABCDEF;
        let entry1 = TTEntry::new(key, None, 100, 50, 10, NodeType::Exact, 0);
        bucket.store(entry1, 0);

        // Try to update with a shallower entry (depth 5)
        let entry2 = TTEntry::new(key, None, 200, 60, 5, NodeType::Exact, 0);
        bucket.store(entry2, 0);

        // Verify the original entry is still there (depth filter should prevent update)
        let result = bucket.probe(key);
        assert!(result.is_some());
        let stored_entry = result.unwrap();
        assert_eq!(stored_entry.depth(), 10);
        assert_eq!(stored_entry.score(), 100);

        // Try to update with a deeper entry (depth 15)
        let entry3 = TTEntry::new(key, None, 300, 70, 15, NodeType::Exact, 0);
        bucket.store(entry3, 0);

        // Verify the new entry replaced the old one
        let result = bucket.probe(key);
        assert!(result.is_some());
        let stored_entry = result.unwrap();
        assert_eq!(stored_entry.depth(), 15);
        assert_eq!(stored_entry.score(), 300);
    }

    // Test depth filter functionality with metrics
    #[test]
    #[cfg(feature = "tt_metrics")]
    fn test_tt_depth_filter_with_metrics() {
        let bucket = bucket::TTBucket::new();
        let metrics = metrics::DetailedTTMetrics::new();

        // Store an entry with depth 10
        let key = 0x1234567890ABCDEF;
        let entry1 = TTEntry::new(key, None, 100, 50, 10, NodeType::Exact, 0);
        bucket.store_with_metrics(entry1, 0, Some(&metrics));

        // Try to update with a shallower entry (depth 5)
        let entry2 = TTEntry::new(key, None, 200, 60, 5, NodeType::Exact, 0);
        bucket.store_with_metrics(entry2, 0, Some(&metrics));

        // The update should be filtered
        assert_eq!(metrics.depth_filtered.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.update_existing.load(Ordering::Relaxed), 0); // No successful update yet

        // Verify the original entry is still there
        let result = bucket.probe(key);
        assert!(result.is_some());
        let stored_entry = result.unwrap();
        assert_eq!(stored_entry.depth(), 10);
        assert_eq!(stored_entry.score(), 100);

        // Try to update with a deeper entry (depth 15)
        let entry3 = TTEntry::new(key, None, 300, 70, 15, NodeType::Exact, 0);
        bucket.store_with_metrics(entry3, 0, Some(&metrics));

        // This update should succeed
        assert_eq!(metrics.depth_filtered.load(Ordering::Relaxed), 1); // Still 1
        assert_eq!(metrics.update_existing.load(Ordering::Relaxed), 1); // Now 1
        assert_eq!(metrics.effective_updates.load(Ordering::Relaxed), 1); // Only the depth-improved update

        // Verify the new entry replaced the old one
        let result = bucket.probe(key);
        assert!(result.is_some());
        let stored_entry = result.unwrap();
        assert_eq!(stored_entry.depth(), 15);
        assert_eq!(stored_entry.score(), 300);
    }

    // Test CAS failure data integrity
    #[test]
    fn test_cas_failure_data_integrity() {
        use std::sync::Arc;
        use std::thread;

        let bucket = Arc::new(bucket::TTBucket::new());
        let num_threads = 4;
        let iterations = 1000;

        // Launch multiple threads trying to write to the same slot
        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let bucket = Arc::clone(&bucket);
                thread::spawn(move || {
                    for i in 0..iterations {
                        let key = 0x1000 + (thread_id as u64);
                        let score = (thread_id * 100 + i) as i16;
                        let entry = TTEntry::new(key, None, score, 0, 10, NodeType::Exact, 0);
                        bucket.store(entry, 0);
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify data integrity - each slot should have consistent key-data pairs
        for i in 0..BUCKET_SIZE {
            let idx = i * 2;
            let key = bucket.entries[idx].load(Ordering::Acquire);

            if key != 0 {
                let data = bucket.entries[idx + 1].load(Ordering::Acquire);
                let entry = TTEntry { key, data };

                // Extract thread_id from key
                let thread_id = (key - 0x1000) as usize;

                // Score should be consistent with the thread that wrote it
                let score = entry.score();
                assert!(
                    score >= (thread_id * 100) as i16 && score < ((thread_id + 1) * 100) as i16,
                    "Score {score} not in expected range for thread {thread_id}"
                );
            }
        }
    }

    #[test]
    fn test_empty_slot_mode() {
        use crate::util::sync_compat::Ordering;

        let tt = TranspositionTable::new(1); // 1MB table

        // Enable empty slot mode
        tt.empty_slot_mode_enabled.store(true, Ordering::Relaxed);

        // Store some entries to fill slots
        for i in 0..4 {
            let hash = 0x1000 + i;
            tt.store(hash, None, 100, 50, 10, NodeType::Exact);
        }

        // Now try to store in a bucket that's full
        // This should be rejected in empty slot mode
        let full_bucket_hash = 0x1000; // Same bucket as first entry
        let new_hash = full_bucket_hash | 0xF000000000000000; // Different hash, same bucket

        tt.store(new_hash, None, 200, 60, 15, NodeType::Exact);

        // The new entry should not be stored (bucket is full and empty slot mode is on)
        let result = tt.probe(new_hash);
        assert!(result.is_none());
    }

    #[test]
    fn test_priority_score_calculation() {
        let entry = TTEntry::new(
            0x1234567890ABCDEF,
            None,
            100,
            50,
            10, // depth
            NodeType::Exact,
            3, // age
        );

        let current_age = 5;
        let score = entry.priority_score(current_age);

        // Score = depth - age_distance + bonuses
        // age_distance = (256 + 5 - 3) & 7 = 2
        // Score = 10 - 2 + 16 (exact bonus) = 24
        assert_eq!(score, 24);
    }

    #[test]
    fn test_flexible_bucket_sizes() {
        // Test small bucket
        let small_tt = TranspositionTable::new_with_config(1, Some(BucketSize::Small));
        assert_eq!(small_tt.bucket_size, Some(BucketSize::Small));

        // Test medium bucket
        let medium_tt = TranspositionTable::new_with_config(16, Some(BucketSize::Medium));
        assert_eq!(medium_tt.bucket_size, Some(BucketSize::Medium));

        // Test large bucket
        let large_tt = TranspositionTable::new_with_config(64, Some(BucketSize::Large));
        assert_eq!(large_tt.bucket_size, Some(BucketSize::Large));

        // Test auto-selection
        let auto_small = TranspositionTable::new_with_config(4, None);
        assert_eq!(auto_small.bucket_size, Some(BucketSize::Small));

        let auto_medium = TranspositionTable::new_with_config(16, None);
        assert_eq!(auto_medium.bucket_size, Some(BucketSize::Medium));

        let auto_large = TranspositionTable::new_with_config(64, None);
        assert_eq!(auto_large.bucket_size, Some(BucketSize::Large));
    }

    #[test]
    fn test_prefetch_functionality() {
        let mut tt = TranspositionTable::new(1);

        // Enable prefetcher
        tt.enable_prefetcher();
        assert!(tt.prefetcher.is_some());

        // Test prefetch operations
        let hash = 0x1234567890ABCDEF;
        tt.prefetch_l1(hash);
        tt.prefetch_l2(hash);
        tt.prefetch_l3(hash);

        // Get stats if available
        if let Some(stats) = tt.prefetch_stats() {
            log::debug!("Prefetch stats: {stats:?}");
        }
    }

    #[test]
    fn test_tt_bucket_store_with_metrics_default() {
        // This test ensures TTBucket::store_with_metrics is referenced in default builds
        let bucket = bucket::TTBucket::new();
        let key = 0xA1B2C3D4E5F60708_u64;
        let entry = TTEntry::new(key, None, 123, 45, 7, NodeType::Exact, 0);

        #[cfg(feature = "tt_metrics")]
        let metrics_none: Option<&metrics::DetailedTTMetrics> = None;
        #[cfg(not(feature = "tt_metrics"))]
        let metrics_none: Option<&()> = None;

        bucket.store_with_metrics(entry, 0, metrics_none);
        let found = bucket.probe(key);
        assert!(found.is_some());
        let e = found.unwrap();
        assert_eq!(e.score(), 123);
        assert_eq!(e.depth(), 7);
    }

    #[test]
    fn test_flexible_bucket_store_with_metrics_default() {
        // This test ensures FlexibleTTBucket::store_with_metrics is referenced in default builds
        let bucket = super::flexible_bucket::FlexibleTTBucket::new(BucketSize::Medium);
        let key = 0x0FEDCBA987654321_u64;
        let entry = TTEntry::new(key, None, 200, 60, 9, NodeType::Exact, 1);

        #[cfg(feature = "tt_metrics")]
        let metrics_none: Option<&metrics::DetailedTTMetrics> = None;
        #[cfg(not(feature = "tt_metrics"))]
        let metrics_none: Option<&()> = None;

        bucket.store_with_metrics(entry, 1, metrics_none);
        let found = bucket.probe(key);
        assert!(found.is_some());
        let e = found.unwrap();
        assert_eq!(e.eval(), 60);
        assert_eq!(e.depth(), 9);
    }

    #[test]
    fn test_flexible_bucket_empty_slot_mode_no_replacement() {
        // Fill a small flexible bucket fully, then attempt to store with empty_slot_mode=true
        let bucket = super::flexible_bucket::FlexibleTTBucket::new(BucketSize::Small); // 4 entries

        // Fill with 4 unique keys
        for i in 0..4u64 {
            let k = 0x1000_0000_0000_0000_u64 | i;
            let e = TTEntry::new(k, None, 100 + i as i16, 0, 8, NodeType::Exact, 0);

            #[cfg(feature = "tt_metrics")]
            let metrics_none: Option<&metrics::DetailedTTMetrics> = None;
            #[cfg(not(feature = "tt_metrics"))]
            let metrics_none: Option<&()> = None;

            bucket.store_with_metrics(e, 0, metrics_none);
        }

        // New key mapping to same bucket layout (independent in FlexibleTTBucket)
        let new_key = 0x2000_0000_0000_0000_u64;
        let new_entry = TTEntry::new(new_key, None, 999, 0, 20, NodeType::Exact, 0);

        #[cfg(feature = "tt_metrics")]
        let metrics_none: Option<&metrics::DetailedTTMetrics> = None;
        #[cfg(not(feature = "tt_metrics"))]
        let metrics_none: Option<&()> = None;

        // Store with empty_slot_mode=true must not replace any entry in a full bucket
        bucket.store_with_metrics_and_mode(new_entry, 0, true, metrics_none);

        // Should not be present
        assert!(bucket.probe(new_key).is_none());
    }
}

#[cfg(test)]
mod parallel_tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn test_parallel_store_and_probe() {
        let tt = Arc::new(TranspositionTable::new(16)); // 16MB table
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = vec![];

        // Launch 8 threads
        for thread_id in 0..8 {
            let tt_clone = Arc::clone(&tt);
            let barrier_clone = Arc::clone(&barrier);

            let handle = thread::spawn(move || {
                barrier_clone.wait();

                // Each thread stores and probes 1000 positions
                for i in 0..1000 {
                    let hash = ((thread_id as u64) << 56) | ((i + 1) as u64);
                    let score = (thread_id * 100 + i) as i16;

                    // Store
                    tt_clone.store(hash, None, score, 0, 10, NodeType::Exact);

                    // Immediately probe
                    let entry = tt_clone.probe(hash);
                    if let Some(found) = entry {
                        // Note: In high concurrency, the score might be from another thread
                        // that updated the same bucket position, so we check if it's valid
                        let found_score = found.score();
                        assert!(
                            (0..=1699).contains(&found_score), // Max: 7*100+999
                            "Found score {found_score} out of valid range"
                        );
                    }
                    // It's acceptable for entry to be None in high concurrency
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }
    }

    /// SplitMix64 hash function for uniform distribution
    #[inline]
    fn splitmix64(mut x: u64) -> u64 {
        x = x.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    #[test]
    fn test_concurrent_updates() {
        let tt = Arc::new(TranspositionTable::new(8)); // 8MB table for better capacity
        let num_threads = 8;
        let updates_per_thread = 10000;
        let shared_positions = 100; // Number of positions all threads will update
        let barrier = Arc::new(Barrier::new(num_threads));

        let mut handles = vec![];

        for thread_id in 0..num_threads {
            let tt_clone = Arc::clone(&tt);
            let barrier_clone = Arc::clone(&barrier);

            let handle = thread::spawn(move || {
                // Wait for all threads to be ready
                barrier_clone.wait();

                // First phase: mostly unique positions
                for i in 0..updates_per_thread * 3 / 4 {
                    let hash = splitmix64(((thread_id as u64) << 32) ^ (i as u64)) | 1;
                    let depth = (i % 20) as u8 + 1;
                    let score = (thread_id * 1000 + i) as i16;
                    tt_clone.store(hash, None, score, 0, depth, NodeType::Exact);
                }

                // Second phase: shared positions with higher depth for better priority
                for i in 0..updates_per_thread / 4 {
                    let pos_idx = (thread_id * (updates_per_thread / 4) + i) % shared_positions;
                    let hash = splitmix64(0x00C0_FFEE_F00D_0000 ^ (pos_idx as u64)) | 1;
                    let depth = 15 + (i % 10) as u8; // Higher depth for better retention
                    let score = (thread_id * 1000 + i) as i16;
                    tt_clone.store(hash, None, score, 0, depth, NodeType::Exact);
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify that most shared positions have valid data
        // Note: TT is a cache and doesn't guarantee all entries are retained
        let mut found_count = 0;
        for i in 0..shared_positions {
            // Use the same hash generation as in the threads
            let hash = splitmix64(0x00C0_FFEE_F00D_0000 ^ (i as u64)) | 1;
            let entry = tt.probe(hash);
            if entry.is_some() {
                found_count += 1;
            }
        }

        // With uniform distribution and better capacity, we expect higher retention rate
        let retention_rate = (found_count as f64) / (shared_positions as f64);
        assert!(
            retention_rate >= 0.95,
            "Expected at least 95% retention rate for shared positions with uniform distribution, got {:.1}% ({found_count}/{shared_positions})",
            retention_rate * 100.0
        );
    }

    #[test]
    fn test_gc_during_search() {
        let tt = Arc::new(TranspositionTable::new(2)); // 2MB table
        let barrier = Arc::new(Barrier::new(4));
        let mut handles = vec![];

        // Launch search threads
        for thread_id in 0..3 {
            let tt_clone = Arc::clone(&tt);
            let barrier_clone = Arc::clone(&barrier);

            let handle = thread::spawn(move || {
                barrier_clone.wait();

                // Continuously store entries
                for i in 0..100000 {
                    let hash = ((thread_id as u64) << 56) | ((i + 1) as u64);
                    tt_clone.store(hash, None, 100, 50, 10, NodeType::Exact);

                    // Periodically check if we can probe old entries
                    if i % 1000 == 0 && i > 0 {
                        let old_hash = ((thread_id as u64) << 56) | ((i - 1000 + 1) as u64);
                        let _entry = tt_clone.probe(old_hash);
                        // Entry might or might not exist due to GC
                    }
                }
            });
            handles.push(handle);
        }

        // Launch GC thread
        {
            let tt_clone = Arc::clone(&tt);
            let barrier_clone = Arc::clone(&barrier);

            let handle = thread::spawn(move || {
                barrier_clone.wait();

                // Periodically run incremental GC
                for _ in 0..100 {
                    thread::sleep(std::time::Duration::from_millis(10));

                    if tt_clone.should_trigger_gc() {
                        while !tt_clone.perform_incremental_gc(128) {
                            // Continue GC
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
    }

    #[test]
    fn test_memory_ordering_stress() {
        // This test verifies that our memory ordering is correct under stress
        let tt = Arc::new(TranspositionTable::new(1));
        let num_threads = 16;
        let iterations = 10000;
        let barrier = Arc::new(Barrier::new(num_threads));

        let mut handles = vec![];

        for thread_id in 0..num_threads {
            let tt_clone = Arc::clone(&tt);
            let barrier_clone = Arc::clone(&barrier);

            let handle = thread::spawn(move || {
                barrier_clone.wait();

                for i in 0..iterations {
                    let hash = 0x1000 + (i % 100) as u64; // Limited number of positions for high contention

                    if thread_id % 2 == 0 {
                        // Writer thread
                        let depth = ((thread_id + i) % 20) as u8 + 1;
                        let score = (thread_id * 100 + i) as i16;
                        tt_clone.store(hash, None, score, score / 2, depth, NodeType::Exact);
                    } else {
                        // Reader thread
                        if let Some(entry) = tt_clone.probe(hash) {
                            // Verify data consistency
                            let score = entry.score();
                            let eval = entry.eval();
                            assert!(
                                eval == score / 2
                                    || eval == (score / 2) + 1
                                    || eval == (score / 2) - 1,
                                "Inconsistent data: score={score}, eval={eval}"
                            );
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
    }
}
