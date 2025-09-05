//! Utility functions for transposition table
//!
//! ## Performance Optimizations
//!
//! ### CAS Retry Strategy
//! - Uses exponential backoff for high-contention scenarios
//! - Maximum 3 retry attempts to prevent excessive spinning
//! - First retry uses `spin_loop()` hint for CPU efficiency
//! - Subsequent retries use minimal exponential backoff (100ns base)
//!
//! ### Memory Ordering
//! - Release/Relaxed ordering for optimal performance on x86/ARM
//! - Atomic operations are carefully minimized for game engine requirements

use super::constants::{DEPTH_MASK, DEPTH_SHIFT};
use super::entry::TTEntry;
#[cfg(feature = "tt_metrics")]
use super::metrics::{record_metric, DetailedTTMetrics, MetricType};
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum number of CAS retry attempts before giving up
const MAX_CAS_RETRIES: usize = 3;

/// Base backoff duration in nanoseconds for failed CAS operations
const BASE_BACKOFF_NS: u64 = 100;

/// Result of update attempt
#[derive(Debug, PartialEq)]
pub(crate) enum UpdateResult {
    Updated,  // Successfully updated
    Filtered, // Filtered out (depth, hashfull, etc.)
    NotFound, // Key not found in this slot
}

/// Get depth threshold based on hashfull - optimized branch version
#[inline(always)]
pub(crate) fn get_depth_threshold(hf: u16) -> u8 {
    // Early return for most common case
    if hf < 600 {
        return 0;
    }

    match hf {
        600..=800 => 2,
        801..=900 => 3,
        901..=950 => 4,
        _ => 5,
    }
}

/// Extract depth from packed data (7 bits)
#[inline(always)]
pub(crate) fn extract_depth(data: u64) -> u8 {
    ((data >> DEPTH_SHIFT) & DEPTH_MASK as u64) as u8
}

/// Generic helper to try updating an existing entry with depth filtering using CAS
#[inline(always)]
pub(crate) fn try_update_entry_generic(
    entries: &[AtomicU64],
    idx: usize,
    old_key: u64,
    new_entry: &TTEntry,
    #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
    #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
) -> UpdateResult {
    if old_key != new_entry.key {
        return UpdateResult::NotFound;
    }

    // Load old data and extract depth efficiently
    let old_data = entries[idx + 1].load(Ordering::Relaxed);

    #[cfg(feature = "tt_metrics")]
    if let Some(m) = metrics {
        record_metric(m, MetricType::AtomicLoad);
    }

    let old_depth = extract_depth(old_data);

    // Skip update if new entry doesn't improve depth
    if new_entry.depth() <= old_depth {
        #[cfg(feature = "tt_metrics")]
        if let Some(m) = metrics {
            record_metric(m, MetricType::DepthFiltered);
        }
        return UpdateResult::Filtered;
    }

    // Use CAS to update data atomically
    // This makes CAS operations more observable for Phase 5 optimization
    #[cfg(feature = "tt_metrics")]
    if let Some(m) = metrics {
        record_metric(m, MetricType::CasAttempt);
    }

    match entries[idx + 1].compare_exchange_weak(
        old_data,
        new_entry.data,
        Ordering::Release,
        Ordering::Relaxed,
    ) {
        Ok(_) => {
            // CAS succeeded - data updated with proper memory ordering
            #[cfg(feature = "tt_metrics")]
            if let Some(m) = metrics {
                record_metric(m, MetricType::CasSuccess);
                record_metric(m, MetricType::UpdateExisting);
                record_metric(m, MetricType::AtomicStore(1)); // Only 1 CAS operation
                record_metric(m, MetricType::EffectiveUpdate);
            }
            UpdateResult::Updated
        }
        Err(current_data) => {
            // CAS failed - implement retry with exponential backoff
            try_update_with_backoff(
                entries,
                idx,
                new_entry,
                current_data,
                #[cfg(feature = "tt_metrics")]
                metrics,
                #[cfg(not(feature = "tt_metrics"))]
                _metrics,
            )
        }
    }
}

/// Optimized CAS retry with exponential backoff for high-contention scenarios
#[inline(always)]
fn try_update_with_backoff(
    entries: &[AtomicU64],
    idx: usize,
    new_entry: &TTEntry,
    mut current_data: u64,
    #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
    #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
) -> UpdateResult {
    for attempt in 1..MAX_CAS_RETRIES {
        // Check if another thread updated with better/equal depth
        if extract_depth(current_data) >= new_entry.depth() {
            #[cfg(feature = "tt_metrics")]
            if let Some(m) = metrics {
                record_metric(m, MetricType::CasFailure);
            }
            return UpdateResult::Filtered;
        }

        // Add CPU-friendly spin hint for first retry
        if attempt == 1 {
            std::hint::spin_loop();
        } else {
            // Exponential backoff for subsequent retries (only in high contention)
            // Keep it minimal to avoid excessive delays in game engines
            std::thread::sleep(std::time::Duration::from_nanos(BASE_BACKOFF_NS << (attempt - 2)));
        }

        // Record CAS attempt for retry
        #[cfg(feature = "tt_metrics")]
        if let Some(m) = metrics {
            record_metric(m, MetricType::CasAttempt);
        }

        // Attempt CAS with fresh current value
        match entries[idx + 1].compare_exchange_weak(
            current_data,
            new_entry.data,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                #[cfg(feature = "tt_metrics")]
                if let Some(m) = metrics {
                    record_metric(m, MetricType::CasSuccess);
                    record_metric(m, MetricType::UpdateExisting);
                    record_metric(m, MetricType::AtomicStore(attempt as u32));
                    record_metric(m, MetricType::EffectiveUpdate);
                }
                return UpdateResult::Updated;
            }
            Err(new_current) => {
                current_data = new_current;
                #[cfg(feature = "tt_metrics")]
                if let Some(m) = metrics {
                    record_metric(m, MetricType::CasFailure);
                }
            }
        }
    }

    // Final attempt exhausted - give up
    #[cfg(feature = "tt_metrics")]
    if let Some(m) = metrics {
        record_metric(m, MetricType::DepthFiltered); // Reuse existing metric type
    }

    UpdateResult::Filtered
}
