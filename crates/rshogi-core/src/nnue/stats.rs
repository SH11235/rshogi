//! NNUE 統計カウンタ（デバッグ・チューニング用）
//!
//! refresh/update 比率の測定に使用。
//! `nnue-stats` feature 有効時のみカウントを行う。
//!
//! # 使用方法
//!
//! ```bash
//! cargo build --release --features nnue-stats
//! ```

#[cfg(feature = "nnue-stats")]
use std::sync::atomic::{AtomicU64, Ordering};

/// NNUE アキュムレータ更新統計
#[cfg(feature = "nnue-stats")]
pub struct NnueStats {
    /// refresh_accumulator 呼び出し回数
    pub refresh_count: AtomicU64,
    /// update_accumulator 呼び出し回数（直前局面からの差分更新）
    pub update_count: AtomicU64,
    /// forward_update_incremental 呼び出し回数（祖先からの複数手差分更新）
    pub forward_update_count: AtomicU64,
    /// evaluate 呼び出し回数
    pub evaluate_count: AtomicU64,
    /// 既に計算済みでスキップされた回数
    pub already_computed_count: AtomicU64,
    /// Finny Tables (AccumulatorCache) ヒット回数（cache valid=true で入った）
    pub cache_hit_count: AtomicU64,
    /// Finny Tables (AccumulatorCache) ミス回数（cache valid=false で full rebuild）
    pub cache_miss_count: AtomicU64,
    /// refresh のうち `refresh_accumulator_with_cache` 経由 (stack 再構築系)
    pub refresh_full_count: AtomicU64,
    /// refresh のうち `update_accumulator_with_cache` の reset path 経由 (玉移動)
    pub refresh_reset_count: AtomicU64,
    /// refresh cache hit 時の差分駒数ヒストグラム
    /// [0]=0個, [1]=1-2, [2]=3-5, [3]=6-10, [4]=11-20, [5]=21-40, [6]=41-80, [7]=81+
    pub refresh_diff_histogram: [AtomicU64; 8],
    /// refresh cache hit 時の差分駒数の累積合計（平均計算用）
    pub refresh_diff_sum: AtomicU64,
}

#[cfg(feature = "nnue-stats")]
impl NnueStats {
    /// 新規作成
    pub const fn new() -> Self {
        Self {
            refresh_count: AtomicU64::new(0),
            update_count: AtomicU64::new(0),
            forward_update_count: AtomicU64::new(0),
            evaluate_count: AtomicU64::new(0),
            already_computed_count: AtomicU64::new(0),
            cache_hit_count: AtomicU64::new(0),
            cache_miss_count: AtomicU64::new(0),
            refresh_full_count: AtomicU64::new(0),
            refresh_reset_count: AtomicU64::new(0),
            refresh_diff_histogram: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            refresh_diff_sum: AtomicU64::new(0),
        }
    }

    /// カウンタをリセット
    pub fn reset(&self) {
        self.refresh_count.store(0, Ordering::Relaxed);
        self.update_count.store(0, Ordering::Relaxed);
        self.forward_update_count.store(0, Ordering::Relaxed);
        self.evaluate_count.store(0, Ordering::Relaxed);
        self.already_computed_count.store(0, Ordering::Relaxed);
        self.cache_hit_count.store(0, Ordering::Relaxed);
        self.cache_miss_count.store(0, Ordering::Relaxed);
        self.refresh_full_count.store(0, Ordering::Relaxed);
        self.refresh_reset_count.store(0, Ordering::Relaxed);
        for bin in &self.refresh_diff_histogram {
            bin.store(0, Ordering::Relaxed);
        }
        self.refresh_diff_sum.store(0, Ordering::Relaxed);
    }

    /// refresh_accumulator 呼び出しをカウント
    #[inline]
    pub fn count_refresh(&self) {
        self.refresh_count.fetch_add(1, Ordering::Relaxed);
    }

    /// update_accumulator 呼び出しをカウント
    #[inline]
    pub fn count_update(&self) {
        self.update_count.fetch_add(1, Ordering::Relaxed);
    }

    /// forward_update_incremental 呼び出しをカウント
    #[inline]
    pub fn count_forward_update(&self) {
        self.forward_update_count.fetch_add(1, Ordering::Relaxed);
    }

    /// evaluate 呼び出しをカウント
    #[inline]
    pub fn count_evaluate(&self) {
        self.evaluate_count.fetch_add(1, Ordering::Relaxed);
    }

    /// already_computed をカウント
    #[inline]
    pub fn count_already_computed(&self) {
        self.already_computed_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Finny Tables cache hit をカウント
    #[inline]
    pub fn count_cache_hit(&self) {
        self.cache_hit_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Finny Tables cache miss をカウント
    #[inline]
    pub fn count_cache_miss(&self) {
        self.cache_miss_count.fetch_add(1, Ordering::Relaxed);
    }

    /// `refresh_accumulator_with_cache` 経由の refresh をカウント
    #[inline]
    pub fn count_refresh_full(&self) {
        self.refresh_full_count.fetch_add(1, Ordering::Relaxed);
    }

    /// `update_accumulator_with_cache` の reset path 経由の refresh をカウント
    #[inline]
    pub fn count_refresh_reset(&self) {
        self.refresh_reset_count.fetch_add(1, Ordering::Relaxed);
    }

    /// refresh cache hit 時の差分駒数を histogram に記録
    #[inline]
    pub fn count_refresh_diff(&self, diff: usize) {
        let bin = match diff {
            0 => 0,
            1..=2 => 1,
            3..=5 => 2,
            6..=10 => 3,
            11..=20 => 4,
            21..=40 => 5,
            41..=80 => 6,
            _ => 7,
        };
        self.refresh_diff_histogram[bin].fetch_add(1, Ordering::Relaxed);
        self.refresh_diff_sum.fetch_add(diff as u64, Ordering::Relaxed);
    }

    /// 統計情報を取得
    pub fn snapshot(&self) -> NnueStatsSnapshot {
        let mut hist = [0u64; 8];
        for (i, bin) in self.refresh_diff_histogram.iter().enumerate() {
            hist[i] = bin.load(Ordering::Relaxed);
        }
        NnueStatsSnapshot {
            refresh_count: self.refresh_count.load(Ordering::Relaxed),
            update_count: self.update_count.load(Ordering::Relaxed),
            forward_update_count: self.forward_update_count.load(Ordering::Relaxed),
            evaluate_count: self.evaluate_count.load(Ordering::Relaxed),
            already_computed_count: self.already_computed_count.load(Ordering::Relaxed),
            cache_hit_count: self.cache_hit_count.load(Ordering::Relaxed),
            cache_miss_count: self.cache_miss_count.load(Ordering::Relaxed),
            refresh_full_count: self.refresh_full_count.load(Ordering::Relaxed),
            refresh_reset_count: self.refresh_reset_count.load(Ordering::Relaxed),
            refresh_diff_histogram: hist,
            refresh_diff_sum: self.refresh_diff_sum.load(Ordering::Relaxed),
        }
    }
}

/// 統計スナップショット
#[derive(Debug, Clone, Copy, Default)]
pub struct NnueStatsSnapshot {
    pub refresh_count: u64,
    pub update_count: u64,
    pub forward_update_count: u64,
    pub evaluate_count: u64,
    pub already_computed_count: u64,
    pub cache_hit_count: u64,
    pub cache_miss_count: u64,
    pub refresh_full_count: u64,
    pub refresh_reset_count: u64,
    pub refresh_diff_histogram: [u64; 8],
    pub refresh_diff_sum: u64,
}

impl NnueStatsSnapshot {
    /// アキュムレータ更新合計
    pub fn total_accumulator_updates(&self) -> u64 {
        self.refresh_count + self.update_count + self.forward_update_count
    }

    /// refresh 率（%）
    pub fn refresh_rate(&self) -> f64 {
        let total = self.total_accumulator_updates();
        if total == 0 {
            0.0
        } else {
            self.refresh_count as f64 / total as f64 * 100.0
        }
    }

    /// update 率（%）
    pub fn update_rate(&self) -> f64 {
        let total = self.total_accumulator_updates();
        if total == 0 {
            0.0
        } else {
            self.update_count as f64 / total as f64 * 100.0
        }
    }

    /// forward_update 率（%）
    pub fn forward_update_rate(&self) -> f64 {
        let total = self.total_accumulator_updates();
        if total == 0 {
            0.0
        } else {
            self.forward_update_count as f64 / total as f64 * 100.0
        }
    }

    /// 差分更新率（update + forward_update）
    pub fn incremental_rate(&self) -> f64 {
        100.0 - self.refresh_rate()
    }

    /// Finny Tables cache の合計アクセス数
    pub fn total_cache_accesses(&self) -> u64 {
        self.cache_hit_count + self.cache_miss_count
    }

    /// Finny Tables cache hit 率（%）
    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.total_cache_accesses();
        if total == 0 {
            0.0
        } else {
            self.cache_hit_count as f64 / total as f64 * 100.0
        }
    }

    /// レポートを出力
    pub fn print_report(&self) {
        let total = self.total_accumulator_updates();
        eprintln!("=== NNUE Accumulator Stats ===");
        eprintln!("evaluate calls:        {:>12}", self.evaluate_count);
        eprintln!("already computed:      {:>12}", self.already_computed_count);
        eprintln!("accumulator updates:   {:>12}", total);
        eprintln!(
            "  refresh:             {:>12} ({:>5.1}%)",
            self.refresh_count,
            self.refresh_rate()
        );
        eprintln!("    full (stack):      {:>12}", self.refresh_full_count);
        eprintln!("    reset (king mv):   {:>12}", self.refresh_reset_count);
        eprintln!(
            "  update (1-step):     {:>12} ({:>5.1}%)",
            self.update_count,
            self.update_rate()
        );
        eprintln!(
            "  forward_update:      {:>12} ({:>5.1}%)",
            self.forward_update_count,
            self.forward_update_rate()
        );
        eprintln!("incremental rate:      {:>11.1}%", self.incremental_rate());
        let cache_total = self.total_cache_accesses();
        eprintln!("finny cache accesses:  {:>12}", cache_total);
        eprintln!(
            "  cache hit:           {:>12} ({:>5.1}%)",
            self.cache_hit_count,
            self.cache_hit_rate()
        );
        eprintln!(
            "  cache miss:          {:>12} ({:>5.1}%)",
            self.cache_miss_count,
            100.0 - self.cache_hit_rate()
        );
        if self.cache_hit_count > 0 {
            let avg = self.refresh_diff_sum as f64 / self.cache_hit_count as f64;
            eprintln!("refresh diff avg:      {:>11.2} pieces/call", avg);
            let labels = ["0", "1-2", "3-5", "6-10", "11-20", "21-40", "41-80", "81+"];
            eprintln!("refresh diff histogram:");
            for (i, &cnt) in self.refresh_diff_histogram.iter().enumerate() {
                let pct = if self.cache_hit_count > 0 {
                    cnt as f64 / self.cache_hit_count as f64 * 100.0
                } else {
                    0.0
                };
                eprintln!("  diff {:>5}:  {:>12} ({:>5.1}%)", labels[i], cnt, pct);
            }
        }
        eprintln!("==============================");
    }
}

// ============================================================================
// Feature有効時: 実際のカウンタ
// ============================================================================

#[cfg(feature = "nnue-stats")]
pub static NNUE_STATS: NnueStats = NnueStats::new();

/// 統計カウンタをリセット
#[cfg(feature = "nnue-stats")]
pub fn reset_nnue_stats() {
    NNUE_STATS.reset();
}

/// 統計スナップショットを取得
#[cfg(feature = "nnue-stats")]
pub fn get_nnue_stats() -> NnueStatsSnapshot {
    NNUE_STATS.snapshot()
}

/// 統計レポートを出力
#[cfg(feature = "nnue-stats")]
pub fn print_nnue_stats() {
    NNUE_STATS.snapshot().print_report();
}

// ============================================================================
// Feature無効時: no-op スタブ
// ============================================================================

/// 統計カウンタをリセット（no-op）
#[cfg(not(feature = "nnue-stats"))]
#[inline]
pub fn reset_nnue_stats() {}

/// 統計スナップショットを取得（空のスナップショット）
#[cfg(not(feature = "nnue-stats"))]
#[inline]
pub fn get_nnue_stats() -> NnueStatsSnapshot {
    NnueStatsSnapshot::default()
}

/// 統計レポートを出力（no-op）
#[cfg(not(feature = "nnue-stats"))]
#[inline]
pub fn print_nnue_stats() {}

// ============================================================================
// インライン統計カウント用マクロ
// ============================================================================

/// refresh カウント（feature有効時のみ）
#[cfg(feature = "nnue-stats")]
macro_rules! count_refresh {
    () => {
        $crate::nnue::stats::NNUE_STATS.count_refresh()
    };
}

/// refresh カウント（no-op）
#[cfg(not(feature = "nnue-stats"))]
macro_rules! count_refresh {
    () => {};
}

/// update カウント（feature有効時のみ）
#[cfg(feature = "nnue-stats")]
macro_rules! count_update {
    () => {
        $crate::nnue::stats::NNUE_STATS.count_update()
    };
}

/// update カウント（no-op）
#[cfg(not(feature = "nnue-stats"))]
macro_rules! count_update {
    () => {};
}

/// forward_update カウント（feature有効時のみ）
#[cfg(feature = "nnue-stats")]
macro_rules! count_forward_update {
    () => {
        $crate::nnue::stats::NNUE_STATS.count_forward_update()
    };
}

/// forward_update カウント（no-op）
#[cfg(not(feature = "nnue-stats"))]
macro_rules! count_forward_update {
    () => {};
}

/// already_computed カウント（feature有効時のみ）
#[cfg(feature = "nnue-stats")]
macro_rules! count_already_computed {
    () => {
        $crate::nnue::stats::NNUE_STATS.count_already_computed()
    };
}

/// already_computed カウント（no-op）
#[cfg(not(feature = "nnue-stats"))]
macro_rules! count_already_computed {
    () => {};
}

/// Finny Tables cache hit カウント（feature有効時のみ）
#[cfg(feature = "nnue-stats")]
macro_rules! count_cache_hit {
    () => {
        $crate::nnue::stats::NNUE_STATS.count_cache_hit()
    };
}

/// Finny Tables cache hit カウント（no-op）
#[cfg(not(feature = "nnue-stats"))]
macro_rules! count_cache_hit {
    () => {};
}

/// Finny Tables cache miss カウント（feature有効時のみ）
#[cfg(feature = "nnue-stats")]
macro_rules! count_cache_miss {
    () => {
        $crate::nnue::stats::NNUE_STATS.count_cache_miss()
    };
}

/// Finny Tables cache miss カウント（no-op）
#[cfg(not(feature = "nnue-stats"))]
macro_rules! count_cache_miss {
    () => {};
}

/// refresh diff 記録（feature有効時のみ）
#[cfg(feature = "nnue-stats")]
macro_rules! count_refresh_diff {
    ($diff:expr) => {
        $crate::nnue::stats::NNUE_STATS.count_refresh_diff($diff)
    };
}

/// refresh diff 記録（no-op）
#[cfg(not(feature = "nnue-stats"))]
macro_rules! count_refresh_diff {
    ($diff:expr) => {
        let _ = $diff;
    };
}

/// refresh_accumulator_with_cache 経由のカウント（feature有効時のみ）
#[cfg(feature = "nnue-stats")]
macro_rules! count_refresh_full {
    () => {
        $crate::nnue::stats::NNUE_STATS.count_refresh_full()
    };
}

#[cfg(not(feature = "nnue-stats"))]
macro_rules! count_refresh_full {
    () => {};
}

/// update_accumulator_with_cache の reset path カウント（feature有効時のみ）
#[cfg(feature = "nnue-stats")]
macro_rules! count_refresh_reset {
    () => {
        $crate::nnue::stats::NNUE_STATS.count_refresh_reset()
    };
}

#[cfg(not(feature = "nnue-stats"))]
macro_rules! count_refresh_reset {
    () => {};
}

pub(crate) use count_already_computed;
pub(crate) use count_cache_hit;
pub(crate) use count_cache_miss;
pub(crate) use count_forward_update;
pub(crate) use count_refresh;
pub(crate) use count_refresh_diff;
pub(crate) use count_refresh_full;
pub(crate) use count_refresh_reset;
pub(crate) use count_update;
