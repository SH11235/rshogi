//! Update operations and helpers for transposition table
//!
//! This module contains helper functions and types for updating
//! transposition table entries with proper synchronization.

use crate::util::sync_compat::{AtomicU64, Ordering};
use super::constants::*;
use super::entry::TTEntry;
#[cfg(feature = "tt_metrics")]
use super::metrics::{DetailedTTMetrics, MetricType, record_metric};

/// Result of update attempt
#[derive(Debug, PartialEq)]
pub(crate) enum UpdateResult {
    Updated,  // Successfully updated
    Filtered, // Filtered out (depth, hashfull, etc.)
    NotFound, // Key not found in this slot
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
    if old_key != new_entry.raw_key() {
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
        new_entry.raw_data(),
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
                    if old_key == new_entry.raw_key() {
                        m.cas_key_match.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                UpdateResult::Filtered
            } else {
                // Retry once if depth is still better
                match entries[idx + 1].compare_exchange_weak(
                    current_data,
                    new_entry.raw_data(),
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