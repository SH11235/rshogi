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
//! - Reader: key は `Acquire` で読み、対応する `data` は `Relaxed` で読む（key の公開と整合）
//! - Writer（空きスロット）: `data` を `Release` → `key` を `Release` で公開
//! - Writer（置換）: `data=0` を `Release` で公開 → `key` を `compare_exchange(AcqRel, Acquire)` で CAS → 最後に `data` を `Release`
//! - これにより、読取側の `Acquire` と書込側の `Release/AcqRel` が対になり、
//!   「key が一致した時点で対応する data は一貫して観測できる」ことを保証します。
//! - Atomic 操作はゲームエンジン要件の範囲で最小化しています。

use super::constants::{DEPTH_MASK, DEPTH_SHIFT};
use super::entry::TTEntry;
#[cfg(feature = "tt_metrics")]
use super::metrics::{record_metric, DetailedTTMetrics, MetricType};
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum number of CAS retry attempts before giving up
const MAX_CAS_RETRIES: usize = 3;

// Backoff: use spin/yield only (no sleeping) for engine responsiveness.
/// Result of update attempt
#[derive(Debug, PartialEq)]
pub(crate) enum UpdateResult {
    Updated,  // Successfully updated
    Filtered, // Filtered out (depth, hashfull, etc.)
    NotFound, // Key not found in this slot
}

/// Result of a replacement attempt on the worst slot
#[derive(Debug, PartialEq)]
pub(crate) enum ReplaceAttemptResult {
    /// Replaced different key with new key+data (CAS succeeded)
    Replaced,
    /// Another thread already inserted the same key; data was updated only
    UpdatedExisting,
    /// CAS failed with different key (race)
    CasFailed,
    /// Observed key changed before zeroing (race detected earlier)
    ObservedMismatch,
}

/// Common replacement attempt: data=0 → key CAS → data=new
/// Returns a `ReplaceAttemptResult` for the caller to decide retry/re-evaluation policy.
#[inline(always)]
pub(crate) fn attempt_replace_worst(
    entries: &[AtomicU64],
    idx: usize, // key at idx, data at idx+1
    old_key: u64,
    new_entry: &TTEntry,
    #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
    #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
) -> ReplaceAttemptResult {
    // Re-read before zeroing to shrink interference window
    let observed = entries[idx].load(Ordering::Acquire);
    if observed != old_key {
        return ReplaceAttemptResult::ObservedMismatch;
    }

    // 1) publish data=0 (invalidates readers by depth==0)
    entries[idx + 1].store(0, Ordering::Release);

    // 2) attempt CAS on key (count attempt here: after re-read and zeroing)
    #[cfg(feature = "tt_metrics")]
    if let Some(m) = metrics {
        record_metric(m, MetricType::CasAttemptKey);
    }

    match entries[idx].compare_exchange(old_key, new_entry.key, Ordering::AcqRel, Ordering::Acquire)
    {
        Ok(_) => {
            // 3) publish final data
            entries[idx + 1].store(new_entry.data, Ordering::Release);
            #[cfg(feature = "tt_metrics")]
            if let Some(m) = metrics {
                record_metric(m, MetricType::CasSuccessKey);
                record_metric(m, MetricType::AtomicStore(1));
                record_metric(m, MetricType::ReplaceWorst);
                if new_entry.depth() > 0 {
                    record_metric(m, MetricType::StoreDepthGT0);
                } else {
                    record_metric(m, MetricType::StoreDepthEQ0);
                }
            }
            ReplaceAttemptResult::Replaced
        }
        Err(current) => {
            if current == new_entry.key {
                // Same key: update data only
                entries[idx + 1].store(new_entry.data, Ordering::Release);
                #[cfg(feature = "tt_metrics")]
                if let Some(m) = metrics {
                    // 同一キー一致を明示
                    m.cas_key_match.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    record_metric(m, MetricType::UpdateExisting);
                    record_metric(m, MetricType::AtomicStore(1));
                    if new_entry.depth() > 0 {
                        record_metric(m, MetricType::StoreDepthGT0);
                    } else {
                        record_metric(m, MetricType::StoreDepthEQ0);
                    }
                }
                ReplaceAttemptResult::UpdatedExisting
            } else {
                #[cfg(feature = "tt_metrics")]
                if let Some(m) = metrics {
                    record_metric(m, MetricType::CasFailure);
                    if entries[idx + 1].load(Ordering::Relaxed) == 0 {
                        record_metric(m, MetricType::CasFailureAfterZero);
                    }
                }
                ReplaceAttemptResult::CasFailed
            }
        }
    }
}

/// Extract depth from packed data (7 bits)
#[inline(always)]
pub(crate) fn extract_depth(data: u64) -> u8 {
    ((data >> DEPTH_SHIFT) & DEPTH_MASK as u64) as u8
}

/// Extract node type from packed data (2 bits)
#[inline(always)]
fn extract_node_type(data: u64) -> crate::search::NodeType {
    use super::constants::{NODE_TYPE_MASK, NODE_TYPE_SHIFT};
    use crate::search::NodeType;

    let raw = ((data >> NODE_TYPE_SHIFT) & NODE_TYPE_MASK as u64) as u8;
    match raw {
        0 => NodeType::Exact,
        1 => NodeType::LowerBound,
        2 => NodeType::UpperBound,
        _ => NodeType::Exact, // Fallback
    }
}

#[inline(always)]
fn extract_pv_flag(data: u64) -> bool {
    use super::constants::{PV_FLAG_MASK, PV_FLAG_SHIFT};
    ((data >> PV_FLAG_SHIFT) & PV_FLAG_MASK) != 0
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
    let old_node_type = extract_node_type(old_data);
    let old_is_pv = extract_pv_flag(old_data);

    // Relaxed depth filtering to improve TT hit rate in parallel search
    // Critical fix for parallel search + iterative deepening:
    // - In parallel search, different threads may reach the same position at different depths
    // - Strict filtering (depth <= old_depth) causes many rejections, reducing hit rate to <1%
    // - Solution: Only filter if new_depth is STRICTLY LESS than old_depth
    // - Same depth updates are allowed (may have better node type or more recent info)
    //
    // This increases hit rate from 0.3% to 20%+ in parallel search scenarios

    // A/B2: PV Exact を Non-PV の bound で上書きしない（深さに関わらず保護）
    // - 既存が PV かつ Exact、かつ新規が Non-PV かつ Bound の場合は更新拒否
    if old_is_pv
        && matches!(old_node_type, crate::search::NodeType::Exact)
        && !new_entry.is_pv()
        && !matches!(new_entry.node_type(), crate::search::NodeType::Exact)
    {
        // Diagnostics: block Non-PV bound from overwriting PV Exact（局面キー短縮付き）
        #[cfg(any(debug_assertions, feature = "tt_diagnostics"))]
        {
            let key_short = format!("{:016x}", new_entry.key());
            log::info!(
                "info string tt_pv_exact_overwrite_blocked=1 key={} old_depth={} new_depth={} old_nt={:?} new_nt={:?}",
                key_short,
                old_depth,
                new_entry.depth(),
                old_node_type,
                new_entry.node_type()
            );
        }
        #[cfg(feature = "tt_metrics")]
        if let Some(m) = metrics {
            record_metric(m, MetricType::DepthFiltered);
        }
        return UpdateResult::Filtered;
    }

    // Filter only if new depth is strictly less than old depth
    if new_entry.depth() < old_depth {
        #[cfg(feature = "tt_metrics")]
        if let Some(m) = metrics {
            record_metric(m, MetricType::DepthFiltered);
        }
        return UpdateResult::Filtered;
    }

    // For same depth, prioritize node type quality (Exact > Lower/Upper)
    // This follows YaneuraOu's replacement policy
    if new_entry.depth() == old_depth {
        use crate::search::NodeType;
        let new_node_type = new_entry.node_type();

        // Prioritize Exact nodes - don't replace Exact with bounds
        if old_node_type == NodeType::Exact && new_node_type != NodeType::Exact {
            #[cfg(feature = "tt_metrics")]
            if let Some(m) = metrics {
                record_metric(m, MetricType::DepthFiltered); // Reuse existing metric
            }
            return UpdateResult::Filtered;
        }

        // For Lower/Upper bounds at same depth, allow replacement
        // (future enhancement: could prioritize tighter bounds)
    }

    // Use CAS to update data atomically
    // This makes CAS operations more observable for Phase 5 optimization
    #[cfg(feature = "tt_metrics")]
    if let Some(m) = metrics {
        record_metric(m, MetricType::CasAttemptData);
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
                record_metric(m, MetricType::CasSuccessData);
                record_metric(m, MetricType::UpdateExisting);
                record_metric(m, MetricType::AtomicStore(1)); // Only 1 CAS operation
                record_metric(m, MetricType::EffectiveUpdate);
                if new_entry.depth() > 0 {
                    record_metric(m, MetricType::StoreDepthGT0);
                } else {
                    record_metric(m, MetricType::StoreDepthEQ0);
                }
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
            // 過度なスリープはエンジンに悪影響のため使用しない。
            // 軽いヒントまたは短い譲歩のみ。
            std::hint::spin_loop();
            std::thread::yield_now();
        }

        // Record CAS attempt for retry
        #[cfg(feature = "tt_metrics")]
        if let Some(m) = metrics {
            record_metric(m, MetricType::CasAttemptData);
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
                    record_metric(m, MetricType::CasSuccessData);
                    record_metric(m, MetricType::UpdateExisting);
                    record_metric(m, MetricType::AtomicStore(attempt as u32));
                    record_metric(m, MetricType::EffectiveUpdate);
                    if new_entry.depth() > 0 {
                        record_metric(m, MetricType::StoreDepthGT0);
                    } else {
                        record_metric(m, MetricType::StoreDepthEQ0);
                    }
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
