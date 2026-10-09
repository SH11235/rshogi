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

/// スレッドセーフなEvalHashエントリ
///
/// AtomicU64×2 + XORエンコーディングによる実装（Stockfish/YaneuraOu準拠）。
///
/// ## 設計原理
/// - `key_xor = key ^ score` として格納
/// - 読み取り時に `key = key_xor ^ score` で復元
/// - 競合状態で片方だけ更新された torn read は、XOR 結果が元の key と一致せず
///   実質的に検出される（キー不一致 = キャッシュミス扱い）
///
/// ## Memory Ordering について
/// Relaxed orderingを使用。これは以下の理由で許容できる：
/// 1. torn read は XOR 照合でほぼ確実にキャッシュミスに落ちる
/// 2. 競合時の「偽陰性」（キャッシュミス）は許容される
/// 3. Stockfish/YaneuraOuも同様のアプローチを採用
///
/// Release/Acquireを使用しない理由：
/// - x86_64では差がない（ハードウェアが強いメモリモデルを提供）
/// - ARMでは追加コストが発生するが、XORエンコーディングで正確性は保証済み
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
    /// XORエンコーディングにより、競合状態での不整合は key 不一致として検出される。
    /// 不一致の場合はキャッシュミスとして扱い、再計算を行う。
    #[inline]
    fn load_pair(&self) -> (u64, u64) {
        // 読み取り順序: key_xor → score
        let key_xor = self.key_xor.load(Ordering::Relaxed);
        let score = self.score.load(Ordering::Relaxed);
        // XORで元のkeyを復元。競合があれば不正なkeyになりキー検証で弾かれる
        (key_xor ^ score, score)
    }

    /// エントリを書き込む
    ///
    /// 書き込み順序は score → key_xor。読み取り側は key_xor → score の順で読む。
    /// 競合状態で片方だけ更新された場合、XOR結果が不整合になり検出可能。
    #[inline]
    fn store_pair(&self, key: u64, score: u64) {
        let key_xor = key ^ score;
        self.score.store(score, Ordering::Relaxed);
        self.key_xor.store(key_xor, Ordering::Relaxed);
    }
}

impl EvalHash {
    /// 通常ページで評価ハッシュを作成する。
    pub fn new(size_mb: usize) -> Self {
        Self::new_with_large_pages(size_mb, false)
    }

    /// 評価ハッシュを作成する。false では通常の Vec による確保を使う。
    pub fn new_with_large_pages(size_mb: usize, large_pages: bool) -> Self {
        let bytes = size_mb.saturating_mul(1024 * 1024);
        let entries = bytes / mem::size_of::<EvalHashEntryAtomic>();
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

    pub fn probe(&self, key: u64) -> Option<i32> {
        // key 0 は未書込 entry (0, 0) と区別できないため常に miss とする
        if self.len == 0 || key == 0 {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        stats::record_probe();
        let entry = &self.table()[self.index(key)];
        let (stored_key, stored_score) = entry.load_pair();
        if stored_key != key {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        stats::record_hit();
        // u64 → u32 → i32: 下位32ビットを符号付き整数として解釈
        // store時に i32 → u32 → u64 と変換しているため、逆変換で元の値を復元
        Some(stored_score as u32 as i32)
    }

    pub fn store(&self, key: u64, score: i32) {
        if self.len == 0 || key == 0 {
            return;
        }
        let idx = self.index(key);
        let entry = &self.table()[idx];
        // i32 → u32 → u64: 符号付き整数をビットパターンを保持したまま拡張
        // probe時に逆変換で元の値を復元する
        entry.store_pair(key, score as u32 as u64);
    }

    /// 全 entry を未書込状態に戻す（in-place。再確保しない）
    ///
    /// 評価設定 (EvalFile / FV_SCALE / bucket routing / MaterialLevel 等) の変更後に
    /// 旧設定の評価値が key 一致で hit するのを防ぐため、TT クリアと同じ箇所で呼ぶ。
    pub fn clear(&self) {
        for entry in self.table() {
            entry.store_pair(0, 0);
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
            assert_eq!(hash.len, (2 << 20) / mem::size_of::<EvalHashEntryAtomic>());
            assert_eq!(hash.mask, hash.len - 1);
            assert_eq!(matches!(hash.storage, EvalHashStorage::Allocated(_)), large_pages);
            if !large_pages {
                assert!(!hash.uses_large_pages());
                assert!(!hash.huge_page_hint_requested());
            }
            assert!(hash.table().iter().all(|entry| entry.load_pair() == (0, 0)));
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
                let score = (key >> 32) as i32;
                hash.prefetch(key);
                hash.store(key, score);
                assert_eq!(hash.probe(key), Some(score));
            }
            for score in [i32::MIN, -1, 0, 1, i32::MAX] {
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
            assert!(hash.table().iter().all(|entry| entry.load_pair() == (0, 0)));
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
}
