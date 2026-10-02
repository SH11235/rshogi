//! Eval hash (evaluation cache) for NNUE.

use std::mem;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::tt::alloc::{AllocKind, Allocation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalHashEntry {
    pub key: u64,
    pub score: i32,
}

pub struct EvalHash {
    ptr: NonNull<EvalHashEntryAtomic>,
    len: usize,
    mask: usize,
    storage: EvalHashStorage,
}

enum EvalHashStorage {
    Allocated(Allocation),
    Regular(Vec<EvalHashEntryAtomic>),
}

// SAFETY: storage が領域を単独所有する。Vec は再確保せず、as_mut_ptr 由来の ptr は
// Vec 自体の移動後も有効。領域は EvalHash の破棄まで生存する。
unsafe impl Send for EvalHash {}
// SAFETY: 公開後のエントリ操作はすべて AtomicU64 を通す。共有中は領域を再確保・解放しない。
unsafe impl Sync for EvalHash {}

/// EvalHashの有効/無効フラグ（グローバル）
///
/// # 注意
/// この変数はプロセス全体で共有されるため、同一プロセス内で複数の
/// Searchインスタンスを使用する場合（対局エンジンと解析機能の同時実行など）、
/// 設定が全インスタンスに影響する。
///
/// 現時点では以下の理由で許容している：
/// - EvalHashキャッシュ自体は各Searchインスタンスで分離されている
/// - 探索結果の正確性には影響しない（パフォーマンスのみ）
/// - 影響は数%のNPS変動程度でユーザー体験への影響は軽微
///
/// 将来、インスタンスごとに異なる設定が必要になった場合は、
/// EvalHash構造体にenabledフラグを持たせる設計に変更すること。
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
pub use stats::{EvalHashStats, eval_hash_stats, reset_eval_hash_stats};

pub fn eval_hash_enabled() -> bool {
    USE_EVAL_HASH.load(Ordering::Relaxed)
}

pub fn set_eval_hash_enabled(enabled: bool) {
    USE_EVAL_HASH.store(enabled, Ordering::Relaxed);
}

// USI EvalHash の指定値は 16 byte/entry 換算の MiB として維持する。
// 同じ指定値で entry 数と index を保ち、実際の表のバイト数だけを半分にする。
const CAPACITY_ENTRY_BYTES: usize = 16;
const VALID_BIT: u64 = 1 << 16;
const KEY_MASK: u64 = !((1 << 17) - 1);

/// 上位 47 bit のキー、valid 1 bit、下位 16 bit の符号付き評価値を持つ。
///
/// 1 本の atomic で全フィールドを同時に読み書きするため torn read は起きない。
/// 他のメモリの公開には使わず、競合時は古い有効値または miss でよいので Relaxed とする。
struct EvalHashEntryAtomic {
    packed: AtomicU64,
}

impl EvalHashEntryAtomic {
    fn new() -> Self {
        Self {
            packed: AtomicU64::new(0),
        }
    }
}

impl EvalHash {
    /// 通常ページで評価ハッシュを作成する。
    /// `size_mb` は 16 byte/entry 換算の容量指定で、表の実容量はその半分になる。
    pub fn new(size_mb: usize) -> Self {
        Self::new_with_large_pages(size_mb, false)
    }

    /// 評価ハッシュを作成する。false では通常の Vec による確保を使う。
    /// `size_mb` は 16 byte/entry 換算の MiB。entry 数は 2 のべき乗に切り下げる。
    pub fn new_with_large_pages(size_mb: usize, large_pages: bool) -> Self {
        let bytes = size_mb.saturating_mul(1024 * 1024);
        let entries = bytes / CAPACITY_ENTRY_BYTES;
        let size = normalize_size(entries);
        let mut storage = if large_pages && size != 0 {
            let bytes = size * mem::size_of::<EvalHashEntryAtomic>();
            let allocation = Allocation::allocate(bytes, mem::align_of::<EvalHashEntryAtomic>());
            // SAFETY: allocation は bytes バイト以上の領域を単独所有し、まだ共有していない。
            // 全ビット 0 は AtomicU64 の有効な初期値であり、各エントリを未書込状態にする。
            // Unix の通常確保は未初期化なので、ページ種別によらずここで初期化する。
            unsafe { allocation.ptr().as_ptr().write_bytes(0, bytes) };
            EvalHashStorage::Allocated(allocation)
        } else {
            let mut table = Vec::with_capacity(size);
            table.resize_with(size, EvalHashEntryAtomic::new);
            EvalHashStorage::Regular(table)
        };
        let ptr = match &mut storage {
            EvalHashStorage::Allocated(allocation) => allocation.ptr().cast(),
            EvalHashStorage::Regular(table) => NonNull::new(table.as_mut_ptr()).unwrap(),
        };

        Self {
            ptr,
            len: size,
            mask: size.saturating_sub(1),
            storage,
        }
    }

    #[inline]
    fn table(&self) -> &[EvalHashEntryAtomic] {
        // SAFETY: ptr は型のアラインメントを満たし、len 個の初期化済みエントリを指す。
        // 空の場合も非 null で整列済み。storage が領域を所有し、返す参照は self より長生きしない。
        // storage 内の Vec から要素を借用・再確保しないため、保持した生ポインタは有効。
        // 共有参照からの変更はエントリ内の atomic 操作に限る。
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    fn allocation_kind(&self) -> AllocKind {
        match &self.storage {
            EvalHashStorage::Allocated(allocation) => allocation.kind(),
            EvalHashStorage::Regular(_) => AllocKind::Regular,
        }
    }

    /// Windows の明示的な Large Pages 確保に成功したかを返す。
    pub fn uses_large_pages(&self) -> bool {
        self.allocation_kind().is_explicit_large_pages()
    }

    /// Linux/Android の huge-page hint 要求に成功したかを返す。実際の配置は OS が決める。
    pub fn huge_page_hint_requested(&self) -> bool {
        self.allocation_kind().is_huge_page_hint()
    }

    /// キーが完全一致する場合にキャッシュ済みの評価値を返す。
    pub fn probe(&self, key: u64) -> Option<i32> {
        if self.len == 0 || key == 0 {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        stats::record_probe();
        if !self.is_cacheable_key(key) {
            return None;
        }
        let entry = &self.table()[self.index(key)];
        let packed = entry.packed.load(Ordering::Relaxed);
        if packed & (KEY_MASK | VALID_BIT) != ((key & KEY_MASK) | VALID_BIT) {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        stats::record_hit();
        Some(i32::from(packed as i16))
    }

    /// i16 に収まり、全キーを照合できる評価値だけを格納する。
    pub fn store(&self, key: u64, score: i32) {
        if self.len == 0 || !self.is_cacheable_key(key) {
            return;
        }
        // NNUE の出力は i16 に clamp されていない。範囲外は再評価に任せる。
        let Ok(score) = i16::try_from(score) else {
            return;
        };
        let idx = self.index(key);
        let entry = &self.table()[idx];
        let packed = (key & KEY_MASK) | VALID_BIT | u64::from(score as u16);
        entry.packed.store(packed, Ordering::Relaxed);
    }

    /// 全 entry を未書込状態に戻す（in-place。再確保しない）
    ///
    /// 評価設定 (EvalFile / FV_SCALE / bucket routing / MaterialLevel 等) の変更後に
    /// 旧設定の評価値が key 一致で hit するのを防ぐため、TT クリアと同じ箇所で呼ぶ。
    pub fn clear(&self) {
        for entry in self.table() {
            entry.packed.store(0, Ordering::Relaxed);
        }
    }

    pub fn prefetch(&self, key: u64) {
        if self.len == 0 {
            return;
        }

        let idx = self.index(key);
        // SAFETY: len は 2 のべき乗で mask == len - 1 のため idx < len。
        // ptr は storage が所有する生存中の領域を指す。
        let entry_ptr = unsafe { self.ptr.as_ptr().add(idx) } as *const u8;

        #[cfg(target_arch = "x86_64")]
        // SAFETY: entry_ptr は生存中のエントリを指し、prefetch は内容を変更しない。
        unsafe {
            use std::arch::x86_64::_mm_prefetch;
            _mm_prefetch(entry_ptr as *const i8, 3);
        }

        #[cfg(target_arch = "aarch64")]
        // SAFETY: entry_ptr は生存中のエントリを指し、prefetch は内容を変更しない。
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

    #[inline]
    fn is_cacheable_key(&self, key: u64) -> bool {
        // index の下位 bit と格納する上位 47 bit で全キーを照合する。
        // 2 MiB 指定以上なら index >= 17 bit なので任意の非ゼロキーを格納できる。
        // 1 MiB 指定は index が 16 bit のため bit 16 が欠ける。この bit が 1 の
        // キーは probe/store とも対象外にして誤ヒットを防ぐ（一様なキーの半数が対象）。
        // key 0 は互換性のため引き続き miss とする。
        key != 0 && key & !(KEY_MASK | self.mask as u64) == 0
    }
}

/// エントリ数を2のべき乗に正規化（切り下げ）
///
/// ハッシュテーブルのサイズは2のべき乗である必要がある（ビットマスクでインデックス計算するため）。
/// 入力値より小さい最大の2のべき乗を返す。
fn normalize_size(entries: usize) -> usize {
    if entries == 0 {
        return 0;
    }
    if entries.is_power_of_two() {
        return entries;
    }
    // checked_next_power_of_two でオーバーフローを安全に処理
    // オーバーフロー時は利用可能な最大サイズにフォールバック
    entries
        .checked_next_power_of_two()
        .map(|n| n / 2)
        .unwrap_or(1 << (usize::BITS - 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_hash_regular_storage_survives_move() {
        let hash = EvalHash::new(1);
        assert!(matches!(hash.storage, EvalHashStorage::Regular(_)));
        hash.store(1, -42);
        let moved = std::sync::Arc::new(hash);
        moved.prefetch(1);
        assert_eq!(moved.probe(1), Some(-42));
        moved.store(2, 123);
        assert_eq!(moved.probe(2), Some(123));
        moved.clear();
        assert_eq!(moved.probe(1), None);
        assert_eq!(moved.probe(2), None);
    }

    #[test]
    fn test_eval_hash_backends_fresh_random_roundtrip_clear() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EvalHash>();
        for large_pages in [false, true] {
            let hash = EvalHash::new_with_large_pages(3, large_pages);
            assert_eq!(hash.len, (2 << 20) / 16);
            assert_eq!(mem::size_of_val(hash.table()), 1 << 20);
            assert_eq!(hash.mask, hash.len - 1);
            assert_eq!(matches!(hash.storage, EvalHashStorage::Allocated(_)), large_pages);
            if !large_pages {
                assert!(!hash.uses_large_pages());
                assert!(!hash.huge_page_hint_requested());
            }
            assert!(hash.table().iter().all(|entry| entry.packed.load(Ordering::Relaxed) == 0));
            let mut key = 0x1234_5678_9abc_def0u64;
            let mut keys = Vec::new();
            for _ in 0..4096 {
                key ^= key << 13;
                key ^= key >> 7;
                key ^= key << 17;
                assert_eq!(hash.probe(key), None);
                keys.push(key);
            }
            for &key in &keys {
                let score = i32::from((key >> 32) as i16);
                hash.prefetch(key);
                hash.store(key, score);
                assert_eq!(hash.probe(key), Some(score));
            }
            for score in [i32::from(i16::MIN), -1, 0, 1, i32::from(i16::MAX)] {
                hash.store(1, score);
                assert_eq!(hash.probe(1), Some(score));
            }
            let collision = 1 + hash.len as u64;
            hash.store(collision, 42);
            assert_eq!(hash.probe(1), None);
            assert_eq!(hash.probe(collision), Some(42));
            hash.store(0, 42);
            assert_eq!(hash.probe(0), None);
            hash.clear();
            assert!(hash.table().iter().all(|entry| entry.packed.load(Ordering::Relaxed) == 0));
            assert!(keys.iter().all(|&key| hash.probe(key).is_none()));
        }
    }

    #[test]
    fn test_eval_hash_backends_zero_size() {
        for large_pages in [false, true] {
            let hash = EvalHash::new_with_large_pages(0, large_pages);
            assert!(hash.table().is_empty());
            assert!(!hash.uses_large_pages());
            assert!(!hash.huge_page_hint_requested());
            hash.store(1, 42);
            hash.prefetch(1);
            hash.clear();
            assert_eq!(hash.probe(1), None);
        }
    }

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

    #[test]
    fn test_eval_hash_size_zero() {
        // サイズ0でも安全に動作すること
        let hash = EvalHash::new(0);
        assert!(hash.table().is_empty());
        assert_eq!(hash.probe(0x1234), None);
        hash.store(0x1234, 100); // パニックしないこと
        hash.prefetch(0x1234); // パニックしないこと
    }

    #[test]
    fn test_eval_hash_collision_overwrite() {
        // 同じインデックスにマッピングされるキーは上書きされる
        let hash = EvalHash::new(1);
        let key1 = 0x0000_0000_0000_0001;
        hash.store(key1, 100);
        assert_eq!(hash.probe(key1), Some(100));

        // 異なるキーで同じエントリを上書き
        let key2 = 0x0000_0001_0000_0001;
        hash.store(key2, 200);

        // key2 は取得できる
        assert_eq!(hash.probe(key2), Some(200));
        assert_eq!(hash.probe(key1), None);
    }

    #[test]
    fn test_eval_hash_boundary_scores() {
        // 境界値テスト
        let hash = EvalHash::new(2);

        // 最大値
        let key1 = 0x1111_1111_1111_1111;
        hash.store(key1, i32::from(i16::MAX));
        assert_eq!(hash.probe(key1), Some(i32::from(i16::MAX)));

        // 最小値
        let key2 = 0x2222_2222_2222_2222;
        hash.store(key2, i32::from(i16::MIN));
        assert_eq!(hash.probe(key2), Some(i32::from(i16::MIN)));

        // ゼロ
        let key3 = 0x3333_3333_3333_3333;
        hash.store(key3, 0);
        assert_eq!(hash.probe(key3), Some(0));

        // 負の値
        let key4 = 0x4444_4444_4444_4444;
        hash.store(key4, -12345);
        assert_eq!(hash.probe(key4), Some(-12345));
    }

    #[test]
    fn test_normalize_size() {
        // 0 → 0
        assert_eq!(normalize_size(0), 0);
        // 2のべき乗はそのまま
        assert_eq!(normalize_size(1), 1);
        assert_eq!(normalize_size(2), 2);
        assert_eq!(normalize_size(4), 4);
        assert_eq!(normalize_size(1024), 1024);
        // 2のべき乗でない場合は切り下げ
        assert_eq!(normalize_size(3), 2);
        assert_eq!(normalize_size(5), 4);
        assert_eq!(normalize_size(1000), 512);
        assert_eq!(normalize_size(1025), 1024);
    }

    #[test]
    fn test_eval_hash_key_zero_never_hits() {
        let hash = EvalHash::new(1);
        hash.store(0, 42);
        assert_eq!(hash.probe(0), None);
    }

    #[test]
    fn test_eval_hash_clear() {
        let hash = EvalHash::new(1);
        let key = 0x1234_5678_9ABC_DEF0;
        hash.store(key, 77);
        assert_eq!(hash.probe(key), Some(77));
        hash.clear();
        assert_eq!(hash.probe(key), None);
    }

    #[test]
    fn test_eval_hash_random_scores_and_same_index_misses() {
        use rand::{Rng, SeedableRng};
        use rand_xoshiro::Xoshiro256PlusPlus;

        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x1234_5678_9abc_def0);
        for large_pages in [false, true] {
            let hash = EvalHash::new_with_large_pages(2, large_pages);
            for _ in 0..4096 {
                let key = rng.random::<u64>() | 1;
                // 各 key の slot を別 key にして、範囲外の store が書き込まないことも確かめる。
                let collision = key ^ (1 << 63);
                assert_eq!(hash.index(key), hash.index(collision));
                for score in [
                    i32::from(rng.random::<i16>()),
                    rng.random_range(i32::MIN..i32::from(i16::MIN)),
                    rng.random_range(i32::from(i16::MAX) + 1..=i32::MAX),
                    i32::from(i16::MIN) - 1,
                    i32::from(i16::MAX) + 1,
                    i32::MIN,
                    i32::MAX,
                ] {
                    hash.store(collision, 42);
                    assert_eq!(hash.probe(key), None);
                    hash.store(key, score);
                    if i16::try_from(score).is_ok() {
                        assert_eq!(hash.probe(key), Some(score));
                        assert_eq!(hash.probe(collision), None);
                    } else {
                        assert_eq!(hash.probe(key), None);
                        assert_eq!(hash.probe(collision), Some(42));
                    }
                }
            }
        }
    }

    #[test]
    fn test_eval_hash_default_capacities_match_every_key_bit() {
        use crate::search::DEFAULT_EVAL_HASH_SIZE_MB;

        assert_eq!(mem::size_of::<EvalHashEntryAtomic>(), 8);
        // core API の既定値と USI の既定値の両方で容量と全 64 bit の照合を確かめる。
        for size_mb in [DEFAULT_EVAL_HASH_SIZE_MB, 256] {
            let hash = EvalHash::new(size_mb);
            assert_eq!(hash.len, size_mb * 1024 * 1024 / 16);
            assert_eq!(mem::size_of_val(hash.table()), size_mb * 1024 * 1024 / 2);
            assert_eq!(KEY_MASK | hash.mask as u64, u64::MAX);
            for key in [1, 0x1234_5678_9abc_def0, u64::MAX] {
                hash.store(key, -321);
                assert_eq!(hash.probe(key), Some(-321));
                for bit in 0..64 {
                    let other = key ^ (1 << bit);
                    assert_eq!(hash.probe(other), None);
                }
                hash.clear();
            }
        }
    }

    #[test]
    fn test_eval_hash_small_table_skips_unverifiable_keys() {
        for large_pages in [false, true] {
            let hash = EvalHash::new_with_large_pages(1, large_pages);
            assert_eq!(hash.len, 1 << 16);
            let key = 1;
            let collision = key | (1 << 16);
            assert_eq!(hash.index(key), hash.index(collision));
            assert_eq!(key & KEY_MASK, collision & KEY_MASK);
            // 有効な key/score がゼロの bit 列でも未書込と区別できる。
            assert_eq!(hash.probe(key), None);
            hash.store(key, 0);
            assert_eq!(hash.probe(key), Some(0));
            assert_eq!(hash.probe(collision), None);
            hash.store(collision, 123);
            assert_eq!(hash.probe(collision), None);
            assert_eq!(hash.probe(key), Some(0));
        }
    }
}
