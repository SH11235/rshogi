//! Eval hash (evaluation cache) for NNUE.

#[cfg(target_arch = "x86_64")]
use std::cell::UnsafeCell;
use std::mem;
#[cfg(not(target_arch = "x86_64"))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalHashEntry {
    pub key: u64,
    pub score: i32,
}

pub struct EvalHash {
    table: Box<[EvalHashEntryAtomic]>,
    mask: usize,
}

static USE_EVAL_HASH: AtomicBool = AtomicBool::new(true);

pub fn eval_hash_enabled() -> bool {
    USE_EVAL_HASH.load(Ordering::Relaxed)
}

pub fn set_eval_hash_enabled(enabled: bool) {
    USE_EVAL_HASH.store(enabled, Ordering::Relaxed);
}

#[cfg(target_arch = "x86_64")]
#[repr(C, align(16))]
struct EvalHashEntryAtomic {
    raw: UnsafeCell<[u64; 2]>,
}

#[cfg(not(target_arch = "x86_64"))]
struct EvalHashEntryAtomic {
    key: AtomicU64,
    score: AtomicU64,
}

#[cfg(target_arch = "x86_64")]
unsafe impl Sync for EvalHashEntryAtomic {}

impl EvalHashEntryAtomic {
    fn new() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            Self {
                raw: UnsafeCell::new([0, 0]),
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            Self {
                key: AtomicU64::new(0),
                score: AtomicU64::new(0),
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    fn load_pair(&self) -> (u64, u64) {
        unsafe {
            use std::arch::x86_64::{_mm_load_si128, _mm_storeu_si128};
            let ptr = self.raw.get() as *const std::arch::x86_64::__m128i;
            let raw = _mm_load_si128(ptr);
            let mut out = mem::MaybeUninit::<[u64; 2]>::uninit();
            _mm_storeu_si128(out.as_mut_ptr() as *mut std::arch::x86_64::__m128i, raw);
            let [key, score] = out.assume_init();
            (key, score)
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    #[inline]
    fn load_pair(&self) -> (u64, u64) {
        let key_xor = self.key.load(Ordering::Relaxed);
        let score = self.score.load(Ordering::Relaxed);
        (key_xor ^ score, score)
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    fn store_pair(&self, key: u64, score: u64) {
        unsafe {
            use std::arch::x86_64::{_mm_set_epi64x, _mm_store_si128};
            let raw = _mm_set_epi64x(score as i64, key as i64);
            let ptr = self.raw.get() as *mut std::arch::x86_64::__m128i;
            _mm_store_si128(ptr, raw);
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    #[inline]
    fn store_pair(&self, key: u64, score: u64) {
        let key_xor = key ^ score;
        self.score.store(score, Ordering::Relaxed);
        self.key.store(key_xor, Ordering::Relaxed);
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
        let entry = &self.table[self.index(key)];
        let (stored_key, stored_score) = entry.load_pair();
        if stored_key != key {
            return None;
        }
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
        // デフォルトで有効
        assert!(eval_hash_enabled());
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
