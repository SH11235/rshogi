//! Utility functions for transposition table

use super::entry::{TTEntry, DEPTH_MASK, DEPTH_SHIFT};
#[cfg(feature = "tt_metrics")]
use super::metrics::{record_metric, DetailedTTMetrics, MetricType};
use crate::util::sync_compat::{AtomicU64, Ordering};

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
        m.cas_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                m.cas_successes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                record_metric(m, MetricType::UpdateExisting);
                record_metric(m, MetricType::AtomicStore(1)); // Only 1 CAS operation
                record_metric(m, MetricType::EffectiveUpdate);
            }
            UpdateResult::Updated
        }
        Err(current_data) => {
            // CAS failed - check if another thread updated with same key
            if extract_depth(current_data) >= new_entry.depth() {
                // Another thread already updated with better/equal depth
                #[cfg(feature = "tt_metrics")]
                if let Some(m) = metrics {
                    m.cas_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    // Check if it was the same key (Phase 5 optimization case)
                    if old_key == new_entry.key {
                        m.cas_key_match.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                UpdateResult::Filtered
            } else {
                // Retry once if depth is still better
                match entries[idx + 1].compare_exchange_weak(
                    current_data,
                    new_entry.data,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        #[cfg(feature = "tt_metrics")]
                        if let Some(m) = metrics {
                            m.cas_successes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            record_metric(m, MetricType::UpdateExisting);
                            record_metric(m, MetricType::AtomicStore(1));
                            record_metric(m, MetricType::EffectiveUpdate);
                        }
                        UpdateResult::Updated
                    }
                    Err(_) => {
                        // Give up after second failure
                        #[cfg(feature = "tt_metrics")]
                        if let Some(m) = metrics {
                            m.cas_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        UpdateResult::Filtered
                    }
                }
            }
        }
    }
}
