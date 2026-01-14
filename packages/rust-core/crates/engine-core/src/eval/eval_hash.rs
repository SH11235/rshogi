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
/// AtomicU64×2 + XORエンコーディングによる実装（Stockfish/YaneuraOu準拠）。
///
/// ## 設計原理
/// - `key_xor = key ^ score` として格納
/// - 読み取り時に `key = key_xor ^ score` で復元
/// - 競合状態で不整合な読み取りが発生した場合、XOR結果が元のkeyと一致しない
/// - キー不一致はキャッシュミスとして扱われ、安全に無視される
///
/// ## Memory Ordering について
/// Relaxed orderingを使用。これは以下の理由で安全：
/// 1. キャッシュの正確性はXORエンコーディングで保証される
/// 2. 競合時の「偽陰性」（キャッシュミス）は許容される
/// 3. 「偽陽性」（誤ったデータを返す）は発生しない（キー検証で排除）
/// 4. Stockfish/YaneuraOuも同様のアプローチを採用
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
        // u64 → u32 → i32: 下位32ビットを符号付き整数として解釈
        // store時に i32 → u32 → u64 と変換しているため、逆変換で元の値を復元
        Some(stored_score as u32 as i32)
    }

    pub fn store(&self, key: u64, score: i32) {
        if self.table.is_empty() {
            return;
        }
        let idx = self.index(key);
        let entry = &self.table[idx];
        // i32 → u32 → u64: 符号付き整数をビットパターンを保持したまま拡張
        // probe時に逆変換で元の値を復元する
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

    #[test]
    fn test_eval_hash_size_zero() {
        // サイズ0でも安全に動作すること
        let hash = EvalHash::new(0);
        assert!(hash.table.is_empty());
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
        // key1 は上書きされてキー不一致でNone（または偶然一致する可能性もある）
        // 同じインデックスで異なるキーの場合、キー検証で弾かれる
    }

    #[test]
    fn test_eval_hash_boundary_scores() {
        // 境界値テスト
        let hash = EvalHash::new(1);

        // 最大値
        let key1 = 0x1111_1111_1111_1111;
        hash.store(key1, i32::MAX);
        assert_eq!(hash.probe(key1), Some(i32::MAX));

        // 最小値
        let key2 = 0x2222_2222_2222_2222;
        hash.store(key2, i32::MIN);
        assert_eq!(hash.probe(key2), Some(i32::MIN));

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
    fn test_eval_hash_key_zero() {
        // キー0でも正常動作
        let hash = EvalHash::new(1);
        hash.store(0, 42);
        assert_eq!(hash.probe(0), Some(42));
    }
}
