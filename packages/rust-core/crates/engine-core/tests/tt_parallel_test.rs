//! Parallel test for TT v2 to verify thread safety with CAS operations

use engine_core::search::tt_v2::{NodeType, TranspositionTableV2};
use std::sync::Arc;
use std::thread;

#[test]
fn test_cas_basic_functionality() {
    // First test basic single-threaded operation
    let tt = TranspositionTableV2::new(1);
    let hash = 0x123456789ABCDEF;

    // Store and retrieve
    tt.store(hash, None, 100, 50, 10, NodeType::Exact);
    let entry = tt.probe(hash);
    assert!(entry.is_some(), "Should find entry after store");
    assert_eq!(entry.unwrap().score(), 100);
}

#[test]
fn test_cas_concurrent_updates() {
    let tt = Arc::new(TranspositionTableV2::new(1));
    let num_threads = 4;
    let test_hash = 0x123456789ABCDEF;

    let mut handles = vec![];

    // Multiple threads updating the same position
    for thread_id in 0..num_threads {
        let tt_clone = Arc::clone(&tt);
        let handle = thread::spawn(move || {
            for i in 0..100 {
                // Each thread stores its ID as the score
                tt_clone.store(
                    test_hash,
                    None,
                    (thread_id * 100 + i) as i16,
                    0,
                    10,
                    NodeType::Exact,
                );

                // Give other threads a chance
                if i % 10 == 0 {
                    thread::yield_now();
                }
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread should complete");
    }

    // Final entry should exist
    let final_entry = tt.probe(test_hash);
    assert!(final_entry.is_some(), "Entry should exist after concurrent updates");
}

#[test]
fn test_cas_different_positions() {
    let tt = Arc::new(TranspositionTableV2::new(1));
    let num_threads = 4;
    let operations_per_thread = 100;

    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let tt_clone = Arc::clone(&tt);
        let handle = thread::spawn(move || {
            // Each thread uses its own hash range
            for i in 0..operations_per_thread {
                let hash = 0x1000000000000000 * (thread_id as u64 + 1) + i as u64;

                // Store entry
                tt_clone.store(hash, None, (thread_id * 100 + i) as i16, 0, 10, NodeType::Exact);

                // Verify we can read it back
                let entry = tt_clone.probe(hash);
                assert!(
                    entry.is_some(),
                    "Entry not found for hash {:#x} (thread {}, iteration {})",
                    hash,
                    thread_id,
                    i
                );
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread should complete");
    }

    // Verify table has entries
    let hashfull = tt.hashfull();
    assert!(hashfull > 0, "Table should contain entries");
}
