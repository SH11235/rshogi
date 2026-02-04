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
        }
    }

    /// カウンタをリセット
    pub fn reset(&self) {
        self.refresh_count.store(0, Ordering::Relaxed);
        self.update_count.store(0, Ordering::Relaxed);
        self.forward_update_count.store(0, Ordering::Relaxed);
        self.evaluate_count.store(0, Ordering::Relaxed);
        self.already_computed_count.store(0, Ordering::Relaxed);
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

    /// 統計情報を取得
    pub fn snapshot(&self) -> NnueStatsSnapshot {
        NnueStatsSnapshot {
            refresh_count: self.refresh_count.load(Ordering::Relaxed),
            update_count: self.update_count.load(Ordering::Relaxed),
            forward_update_count: self.forward_update_count.load(Ordering::Relaxed),
            evaluate_count: self.evaluate_count.load(Ordering::Relaxed),
            already_computed_count: self.already_computed_count.load(Ordering::Relaxed),
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

pub(crate) use count_already_computed;
pub(crate) use count_forward_update;
pub(crate) use count_refresh;
pub(crate) use count_update;
