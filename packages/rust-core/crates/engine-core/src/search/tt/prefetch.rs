//! CPU cache prefetch utilities for transposition table

use crate::util::sync_compat::{AtomicU64, Ordering};

// Architecture-specific imports for x86/x86_64
#[cfg(target_arch = "x86")]
use std::arch::x86::{_mm_prefetch, _MM_HINT_NTA, _MM_HINT_T0, _MM_HINT_T1, _MM_HINT_T2};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{_mm_prefetch, _MM_HINT_NTA, _MM_HINT_T0, _MM_HINT_T1, _MM_HINT_T2};

/// Statistics for prefetch operations
#[derive(Debug, Clone, Copy)]
pub struct PrefetchStats {
    pub calls: u64,    // Number of prefetch calls
    pub hits: u64,     // Kept for compatibility (same as calls for now)
    pub misses: u64,
    pub hit_rate: f64,
    pub distance: usize,
}

/// Adaptive prefetcher that tracks hit/miss rates
pub(crate) struct AdaptivePrefetcher {
    hits: AtomicU64,
    misses: AtomicU64,
}

impl AdaptivePrefetcher {
    /// Create new adaptive prefetcher
    pub fn new() -> Self {
        AdaptivePrefetcher {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Record a prefetch call
    pub fn record_call(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a miss
    #[allow(dead_code)]
    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Get statistics
    pub fn stats(&self) -> PrefetchStats {
        let calls = self.hits.load(Ordering::Relaxed); // Currently tracking calls
        let misses = self.misses.load(Ordering::Relaxed);
        let total = calls + misses;
        let hit_rate = if total > 0 {
            calls as f64 / total as f64
        } else {
            0.0
        };

        PrefetchStats {
            calls,
            hits: calls, // Keep for compatibility
            misses,
            hit_rate,
            distance: 1, // Default distance, could be made configurable
        }
    }
}

/// Prefetch memory into cache with specified hint
///
/// # Arguments
/// * `addr` - The memory address to prefetch
/// * `hint` - Prefetch hint (0-3):
///   - 0: Non-temporal (bypass cache)
///   - 1: L3 cache
///   - 2: L2 cache  
///   - 3: L1 cache
#[inline(always)]
pub(crate) fn prefetch_memory(addr: *const u8, hint: i32) {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    unsafe {
        match hint {
            0 => _mm_prefetch(addr as *const i8, _MM_HINT_NTA), // Non-temporal
            1 => _mm_prefetch(addr as *const i8, _MM_HINT_T2),  // L3
            2 => _mm_prefetch(addr as *const i8, _MM_HINT_T1),  // L2
            3 => _mm_prefetch(addr as *const i8, _MM_HINT_T0),  // L1
            _ => {}                                             // Invalid hint, do nothing
        }
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
    {
        // No prefetch available on this architecture
        let _ = (addr, hint);
    }
}

/// Prefetch multiple cache lines for larger data structures
///
/// NOTE: Assumes 64-byte cache lines (x86/x86_64). Adjust if targeting different architectures.
///
/// # Arguments
/// * `addr` - The base memory address
/// * `cache_lines` - Number of cache lines to prefetch (each is 64 bytes)
/// * `hint` - Prefetch hint (0-3)
#[inline(always)]
pub(crate) fn prefetch_multiple(addr: *const u8, cache_lines: usize, hint: i32) {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    unsafe {
        // Early return for invalid hints
        if !(0..=3).contains(&hint) {
            return;
        }

        for i in 0..cache_lines {
            let offset_addr = addr.add(i * 64) as *const i8;

            match hint {
                0 => _mm_prefetch(offset_addr, _MM_HINT_NTA),
                1 => _mm_prefetch(offset_addr, _MM_HINT_T2),
                2 => _mm_prefetch(offset_addr, _MM_HINT_T1),
                _ => _mm_prefetch(offset_addr, _MM_HINT_T0), // hint 3 and default
            }
        }
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
    {
        // No prefetch available on this architecture
        let _ = (addr, cache_lines, hint);
    }
}
