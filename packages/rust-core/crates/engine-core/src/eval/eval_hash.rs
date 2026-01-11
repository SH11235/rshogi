//! Eval hash (evaluation cache) for NNUE.

use std::mem;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalHashEntry {
    pub key: u64,
    pub score: i32,
}

pub struct EvalHash {
    table: Box<[EvalHashEntryAtomic]>,
    mask: usize,
}

static USE_EVAL_HASH: AtomicBool = AtomicBool::new(false);

// ============================================================================
// 統計機能（diagnostics フィーチャー有効時のみ）
// ============================================================================

#[cfg(feature = "diagnostics")]
mod stats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static PROBE_COUNT: AtomicU64 = AtomicU64::new(0);
    static HIT_COUNT: AtomicU64 = AtomicU64::new(0);

    /// EvalHash の統計情報
    #[derive(Debug, Clone, Copy)]
    pub struct EvalHashStats {
        pub probes: u64,
        pub hits: u64,
    }

    impl EvalHashStats {
        /// ヒット率を計算（0.0 - 1.0）
        pub fn hit_rate(&self) -> f64 {
            if self.probes == 0 {
                0.0
            } else {
                self.hits as f64 / self.probes as f64
            }
        }

        /// ヒット率をパーセントで取得
        pub fn hit_rate_percent(&self) -> f64 {
            self.hit_rate() * 100.0
        }
    }

    impl std::fmt::Display for EvalHashStats {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "probes: {}, hits: {}, hit_rate: {:.2}%",
                self.probes,
                self.hits,
                self.hit_rate_percent()
            )
        }
    }

    /// 統計情報を取得
    pub fn eval_hash_stats() -> EvalHashStats {
        EvalHashStats {
            probes: PROBE_COUNT.load(Ordering::Relaxed),
            hits: HIT_COUNT.load(Ordering::Relaxed),
        }
    }

    /// 統計情報をリセット
    pub fn reset_eval_hash_stats() {
        PROBE_COUNT.store(0, Ordering::Relaxed);
        HIT_COUNT.store(0, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_probe() {
        PROBE_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_hit() {
        HIT_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(feature = "diagnostics")]
pub use stats::{eval_hash_stats, reset_eval_hash_stats, EvalHashStats};

pub fn eval_hash_enabled() -> bool {
    USE_EVAL_HASH.load(Ordering::Relaxed)
}

pub fn set_eval_hash_enabled(enabled: bool) {
    USE_EVAL_HASH.store(enabled, Ordering::Relaxed);
}

/// スレッドセーフなEvalHashエントリ
///
/// AtomicU64×2 + XORエンコーディングによる実装。
/// - key_xor = key ^ score として格納
/// - 読み取り時に key = key_xor ^ score で復元
/// - torn read（部分的な読み取り）が発生した場合、keyが不一致となり検出可能
///
/// Relaxed orderingを使用し、最小限のオーバーヘッドで同期を実現。
struct EvalHashEntryAtomic {
    key_xor: AtomicU64,
    score: AtomicU64,
}

impl EvalHashEntryAtomic {
    fn new() -> Self {
        Self {
            key_xor: AtomicU64::new(0),
            score: AtomicU64::new(0),
        }
    }

    /// エントリを読み取り、(key, score) を返す
    ///
    /// XORエンコーディングにより、torn readは key 不一致として検出される
    #[inline]
    fn load_pair(&self) -> (u64, u64) {
        let key_xor = self.key_xor.load(Ordering::Relaxed);
        let score = self.score.load(Ordering::Relaxed);
        (key_xor ^ score, score)
    }

    /// エントリを書き込む
    ///
    /// key ^ score を key_xor として格納することで、
    /// 部分的な書き込みが読み取られた場合でもkey不一致で検出可能
    #[inline]
    fn store_pair(&self, key: u64, score: u64) {
        let key_xor = key ^ score;
        // score を先に書き込み、次に key_xor を書き込む
        // 読み取り側は key_xor, score の順で読むため、
        // 不整合があれば XOR 結果が元の key と一致しない
        self.score.store(score, Ordering::Relaxed);
        self.key_xor.store(key_xor, Ordering::Relaxed);
    }
}

impl EvalHash {
    pub fn new(size_mb: usize) -> Self {
        let bytes = size_mb.saturating_mul(1024 * 1024);
        let entries = bytes / mem::size_of::<EvalHashEntryAtomic>();
        let size = normalize_size(entries);
        let mut table = Vec::with_capacity(size);
        table.resize_with(size, EvalHashEntryAtomic::new);

        Self {
            table: table.into_boxed_slice(),
            mask: size.saturating_sub(1),
        }
    }

    pub fn clear(&self) {
        let len = self.table.len();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

        if threads <= 1 || len < threads * 1024 {
            unsafe {
                std::ptr::write_bytes(self.table.as_ptr() as *mut EvalHashEntryAtomic, 0, len);
            }
            return;
        }

        let chunk = len.div_ceil(threads);
        let ptr = self.table.as_ptr();

        std::thread::scope(|scope| {
            for i in 0..threads {
                let start = i * chunk;
                if start >= len {
                    break;
                }
                let end = (start + chunk).min(len);
                let count = end - start;
                let ptr_addr = unsafe { ptr.add(start) } as usize;

                scope.spawn(move || unsafe {
                    let ptr = ptr_addr as *mut EvalHashEntryAtomic;
                    std::ptr::write_bytes(ptr, 0, count);
                });
            }
        });
    }

    pub fn probe(&self, key: u64) -> Option<i32> {
        if self.table.is_empty() {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        stats::record_probe();
        let entry = &self.table[self.index(key)];
        let (stored_key, stored_score) = entry.load_pair();
        if stored_key != key {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        stats::record_hit();
        Some(stored_score as u32 as i32)
    }

    pub fn store(&self, key: u64, score: i32) {
        if self.table.is_empty() {
            return;
        }
        let idx = self.index(key);
        let entry = &self.table[idx];
        entry.store_pair(key, score as u32 as u64);
    }

    pub fn prefetch(&self, key: u64) {
        if self.table.is_empty() {
            return;
        }

        let idx = self.index(key);
        let entry_ptr = unsafe { self.table.as_ptr().add(idx) } as *const u8;

        #[cfg(target_arch = "x86_64")]
        unsafe {
            use std::arch::x86_64::_mm_prefetch;
            _mm_prefetch(entry_ptr as *const i8, 3);
        }

        #[cfg(target_arch = "aarch64")]
        unsafe {
            use std::arch::aarch64::_prefetch;
            _prefetch(entry_ptr as *const i8, 0, 3);
        }

        #[cfg(all(not(target_arch = "x86_64"), not(target_arch = "aarch64")))]
        let _ = entry_ptr;
    }

    #[inline]
    fn index(&self, key: u64) -> usize {
        (key as usize) & self.mask
    }
}

fn normalize_size(entries: usize) -> usize {
    if entries == 0 {
        return 0;
    }
    if entries.is_power_of_two() {
        return entries;
    }
    entries.next_power_of_two() / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_hash_store_probe() {
        let hash = EvalHash::new(1);
        let key = 0x1234_5678_9ABC_DEF0;
        let score = -321;
        let entry = EvalHashEntry { key, score };
        assert_eq!(entry.key, key);
        assert_eq!(hash.probe(key), None);

        hash.store(key, score);
        assert_eq!(hash.probe(key), Some(score));
    }

    #[test]
    fn test_eval_hash_clear() {
        let hash = EvalHash::new(1);
        let key = 0x0FED_CBA9_8765_4321;
        hash.store(key, 42);
        hash.clear();
        assert_eq!(hash.probe(key), None);
    }

    #[test]
    fn test_eval_hash_size_power_of_two() {
        let hash = EvalHash::new(3);
        assert!(hash.table.len().is_power_of_two() || hash.table.is_empty());
    }

    #[test]
    fn test_eval_hash_enabled_default() {
        // デフォルトで無効
        assert!(!eval_hash_enabled());
    }

    #[test]
    fn test_eval_hash_enabled_toggle() {
        let original = eval_hash_enabled();
        set_eval_hash_enabled(false);
        assert!(!eval_hash_enabled());
        set_eval_hash_enabled(true);
        assert!(eval_hash_enabled());
        // 元に戻す
        set_eval_hash_enabled(original);
    }
}
