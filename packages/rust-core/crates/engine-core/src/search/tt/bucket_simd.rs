//! SIMD-optimized bucket operations for transposition table

use super::constants::BUCKET_SIZE;
use super::entry::TTEntry;
use crate::search::NodeType;
use crate::util::sync_compat::{AtomicU64, Ordering};

/// SIMD-optimized probe implementation for TTBucket
pub(crate) fn probe_simd(
    entries: &[AtomicU64; BUCKET_SIZE * 2],
    target_key: u64,
) -> Option<TTEntry> {
    // Load all 4 keys at once for SIMD comparison
    // Use Acquire ordering for key loads to ensure proper synchronization with Release stores
    let mut keys = [0u64; BUCKET_SIZE];
    for (i, key) in keys.iter_mut().enumerate() {
        *key = entries[i * 2].load(Ordering::Acquire);
    }

    // Use SIMD to find matching key
    if let Some(idx) = crate::search::tt_simd::simd::find_matching_key(&keys, target_key) {
        // Use Relaxed for data since Acquire on key already synchronized
        let data = entries[idx * 2 + 1].load(Ordering::Relaxed);
        let entry = TTEntry {
            key: keys[idx],
            data,
        };

        if entry.depth() > 0 {
            return Some(entry);
        }
    }

    None
}

/// Find worst entry using SIMD priority calculation
pub(crate) fn find_worst_entry_simd(
    entries: &[AtomicU64; BUCKET_SIZE * 2],
    current_age: u8,
) -> (usize, i32) {
    // Prepare data for SIMD priority calculation
    let mut depths = [0u8; BUCKET_SIZE];
    let mut ages = [0u8; BUCKET_SIZE];
    let mut is_pv = [false; BUCKET_SIZE];
    let mut is_exact = [false; BUCKET_SIZE];
    let mut is_empty = [false; BUCKET_SIZE];

    // Load all entries at once
    // Use Acquire ordering on key load to ensure we see consistent data
    for i in 0..BUCKET_SIZE {
        let idx = i * 2;
        let key = entries[idx].load(Ordering::Acquire);
        if key == 0 {
            // Mark empty slots
            is_empty[i] = true;
            depths[i] = 0;
            ages[i] = 0;
            is_pv[i] = false;
            is_exact[i] = false;
        } else {
            // Use Relaxed for data since Acquire on key already synchronized
            let data = entries[idx + 1].load(Ordering::Relaxed);
            let entry = TTEntry { key, data };
            depths[i] = entry.depth();
            ages[i] = entry.age();
            is_pv[i] = entry.is_pv();
            is_exact[i] = entry.node_type() == NodeType::Exact;
        }
    }

    // Calculate all priority scores using SIMD
    let mut scores = crate::search::tt_simd::simd::calculate_priority_scores(
        &depths,
        &ages,
        &is_pv,
        &is_exact,
        current_age,
    );

    // Set empty entries to minimum priority (they should be replaced first)
    for (i, empty) in is_empty.iter().enumerate() {
        if *empty {
            scores[i] = i32::MIN;
        }
    }

    // Find minimum score and its index
    let mut worst_idx = 0;
    let mut worst_score = scores[0];
    for (i, &score) in scores.iter().enumerate().skip(1) {
        if score < worst_score {
            worst_score = score;
            worst_idx = i;
        }
    }

    (worst_idx, worst_score)
}
