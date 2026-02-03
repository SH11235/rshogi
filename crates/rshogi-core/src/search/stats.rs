//! 探索統計（search-stats feature有効時のみ）
//!
//! 探索中の各種枝刈りの発生回数を記録し、チューニングやデバッグに使用する。

/// 深度別統計の最大深度
#[cfg(feature = "search-stats")]
pub(super) const STATS_MAX_DEPTH: usize = 32;

/// 探索統計カウンタ
///
/// 各枝刈りの発生回数を記録し、チューニングやデバッグに使用する。
/// `search-stats` featureが有効な場合のみコンパイルされる。
#[cfg(feature = "search-stats")]
#[derive(Debug, Clone)]
pub struct SearchStats {
    /// 総ノード数（探索関数の呼び出し回数）
    pub nodes_searched: u64,
    /// LMR適用回数
    pub lmr_applied: u64,
    /// LMRによる再探索回数
    pub lmr_research: u64,
    /// Move Loop内の枝刈り回数（LMP, Futility, SEE, History等の合計）
    pub move_loop_pruned: u64,
    /// Futility Pruning（静的評価による枝刈り）回数
    pub futility_pruned: u64,
    /// NMP（Null Move Pruning）試行回数
    pub nmp_attempted: u64,
    /// NMPによる枝刈り成功回数
    pub nmp_cutoff: u64,
    /// Razoring適用回数
    pub razoring_applied: u64,
    /// ProbCut試行回数
    pub probcut_attempted: u64,
    /// ProbCutによる枝刈り成功回数
    pub probcut_cutoff: u64,
    /// Singular Extension適用回数
    pub singular_extension: u64,
    /// Multi-Cut発動回数
    pub multi_cut: u64,
    /// TT（置換表）カットオフ回数
    pub tt_cutoff: u64,
    /// 深度別ノード数（depth 0-31）
    pub nodes_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別TTカットオフ数
    pub tt_cutoff_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別TTプローブ数
    pub tt_probe_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別TTヒット数
    pub tt_hit_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別TT深度不足でカットオフ失敗
    pub tt_fail_depth_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別TTバウンド不適合でカットオフ失敗
    pub tt_fail_bound_by_depth: [u64; STATS_MAX_DEPTH],
    /// LMRでdepth 1に遷移したノード数（親の深度別）
    pub lmr_to_depth1_from: [u64; STATS_MAX_DEPTH],
    /// depth 1での全子ノード数（統計用）
    pub depth1_children_total: u64,
    /// depth 1でTTカットオフされた子ノード数
    pub depth1_children_tt_cut: u64,
    /// 深度別TT書き込み数
    pub tt_write_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別Razoring適用回数
    pub razoring_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別Futility Pruning適用回数
    pub futility_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別NMPカットオフ回数
    pub nmp_cutoff_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別first move cutoff回数（Move Ordering品質）
    pub first_move_cutoff_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別カットオフ回数（first move cutoff rate計算用）
    pub cutoff_by_depth: [u64; STATS_MAX_DEPTH],
    /// 深度別のカットオフ時move_count合計（平均計算用）
    pub move_count_sum_by_depth: [u64; STATS_MAX_DEPTH],
    /// LMR削減量（r/1024）のヒストグラム（0-15+）
    pub lmr_reduction_histogram: [u64; 16],
    /// LMR適用後の新深度別ノード数
    pub lmr_new_depth_histogram: [u64; STATS_MAX_DEPTH],
}

#[cfg(feature = "search-stats")]
impl Default for SearchStats {
    fn default() -> Self {
        Self {
            nodes_searched: 0,
            lmr_applied: 0,
            lmr_research: 0,
            move_loop_pruned: 0,
            futility_pruned: 0,
            nmp_attempted: 0,
            nmp_cutoff: 0,
            razoring_applied: 0,
            probcut_attempted: 0,
            probcut_cutoff: 0,
            singular_extension: 0,
            multi_cut: 0,
            tt_cutoff: 0,
            nodes_by_depth: [0; STATS_MAX_DEPTH],
            tt_cutoff_by_depth: [0; STATS_MAX_DEPTH],
            tt_probe_by_depth: [0; STATS_MAX_DEPTH],
            tt_hit_by_depth: [0; STATS_MAX_DEPTH],
            tt_fail_depth_by_depth: [0; STATS_MAX_DEPTH],
            tt_fail_bound_by_depth: [0; STATS_MAX_DEPTH],
            lmr_to_depth1_from: [0; STATS_MAX_DEPTH],
            depth1_children_total: 0,
            depth1_children_tt_cut: 0,
            tt_write_by_depth: [0; STATS_MAX_DEPTH],
            razoring_by_depth: [0; STATS_MAX_DEPTH],
            futility_by_depth: [0; STATS_MAX_DEPTH],
            nmp_cutoff_by_depth: [0; STATS_MAX_DEPTH],
            first_move_cutoff_by_depth: [0; STATS_MAX_DEPTH],
            cutoff_by_depth: [0; STATS_MAX_DEPTH],
            move_count_sum_by_depth: [0; STATS_MAX_DEPTH],
            lmr_reduction_histogram: [0; 16],
            lmr_new_depth_histogram: [0; STATS_MAX_DEPTH],
        }
    }
}

#[cfg(feature = "search-stats")]
impl SearchStats {
    /// 統計をリセット
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// 統計をフォーマットして文字列として返す
    pub fn format_report(&self) -> String {
        let mut report = String::new();
        report.push_str("=== Search Statistics ===\n");
        report.push_str(&format!("Nodes searched:      {:>12}\n", self.nodes_searched));
        report.push_str(&format!("TT cutoffs:          {:>12}\n", self.tt_cutoff));
        report.push_str("--- Pre-Move Pruning ---\n");
        report.push_str(&format!("NMP attempted:       {:>12}\n", self.nmp_attempted));
        report.push_str(&format!("NMP cutoffs:         {:>12}\n", self.nmp_cutoff));
        report.push_str(&format!("Razoring:            {:>12}\n", self.razoring_applied));
        report.push_str(&format!("Futility (static):   {:>12}\n", self.futility_pruned));
        report.push_str(&format!("ProbCut attempted:   {:>12}\n", self.probcut_attempted));
        report.push_str(&format!("ProbCut cutoffs:     {:>12}\n", self.probcut_cutoff));
        report.push_str("--- Move Loop ---\n");
        report.push_str(&format!("Move loop pruned:    {:>12}\n", self.move_loop_pruned));
        report.push_str(&format!("LMR applied:         {:>12}\n", self.lmr_applied));
        report.push_str(&format!("LMR re-search:       {:>12}\n", self.lmr_research));
        report.push_str("--- Extensions ---\n");
        report.push_str(&format!("Singular extension:  {:>12}\n", self.singular_extension));
        report.push_str(&format!("Multi-cut:           {:>12}\n", self.multi_cut));
        // 深度別ノード数（ノード数が0より大きい深度のみ表示）
        report.push_str("--- Nodes by Depth ---\n");
        for (d, &count) in self.nodes_by_depth.iter().enumerate() {
            if count > 0 {
                let tt_cut = self.tt_cutoff_by_depth[d];
                let tt_rate = if count > 0 {
                    (tt_cut as f64 / count as f64 * 100.0) as u32
                } else {
                    0
                };
                report.push_str(&format!(
                    "  depth {:>2}: {:>10} nodes, {:>8} TT cuts ({:>2}%)\n",
                    d, count, tt_cut, tt_rate
                ));
            }
        }
        // TT詳細統計（depth 1のみ詳細表示）
        report.push_str("--- TT Details (depth 1) ---\n");
        let probe = self.tt_probe_by_depth[1];
        let hit = self.tt_hit_by_depth[1];
        let cut = self.tt_cutoff_by_depth[1];
        let fail_depth = self.tt_fail_depth_by_depth[1];
        let fail_bound = self.tt_fail_bound_by_depth[1];
        if probe > 0 {
            report.push_str(&format!(
                "  Probes: {}, Hits: {} ({:.1}%), Cuts: {} ({:.1}%)\n",
                probe,
                hit,
                hit as f64 / probe as f64 * 100.0,
                cut,
                cut as f64 / probe as f64 * 100.0
            ));
            report
                .push_str(&format!("  Fail reasons: depth={}, bound={}\n", fail_depth, fail_bound));
        }
        // depth 1への遷移元分析
        report.push_str("--- LMR to Depth 1 Sources ---\n");
        for (d, &count) in self.lmr_to_depth1_from.iter().enumerate() {
            if count > 0 {
                report.push_str(&format!("  from depth {:>2}: {:>8} nodes\n", d, count));
            }
        }
        // TT書き込み統計
        report.push_str("--- TT Writes by Depth ---\n");
        for (d, &count) in self.tt_write_by_depth.iter().enumerate() {
            if count > 0 {
                let probe = self.tt_probe_by_depth[d];
                let ratio = if probe > 0 {
                    format!("{:.1}x", count as f64 / probe as f64)
                } else {
                    "-".to_string()
                };
                report.push_str(&format!(
                    "  depth {:>2}: {:>8} writes (probe ratio: {})\n",
                    d, count, ratio
                ));
            }
        }
        // 早期リターン統計（depth別）
        report.push_str("--- Early Return by Depth ---\n");
        for d in 0..STATS_MAX_DEPTH {
            let razoring = self.razoring_by_depth[d];
            let futility = self.futility_by_depth[d];
            let nmp = self.nmp_cutoff_by_depth[d];
            let nodes = self.nodes_by_depth[d];
            if razoring > 0 || futility > 0 || nmp > 0 {
                report.push_str(&format!(
                    "  depth {:>2}: razoring={:>6}, futility={:>6}, nmp={:>6} (nodes={})\n",
                    d, razoring, futility, nmp, nodes
                ));
            }
        }
        // Move Ordering品質統計（depth別）
        report.push_str("--- Move Ordering Quality (First Move Cutoff Rate) ---\n");
        for d in 0..STATS_MAX_DEPTH {
            let first_cut = self.first_move_cutoff_by_depth[d];
            let total_cut = self.cutoff_by_depth[d];
            if total_cut > 0 {
                let rate = first_cut as f64 / total_cut as f64 * 100.0;
                report.push_str(&format!(
                    "  depth {:>2}: {:>6}/{:>6} ({:>5.1}%)\n",
                    d, first_cut, total_cut, rate
                ));
            }
        }
        // カットオフ時のmove_count平均（depth別）
        report.push_str("--- Average Move Count at Cutoff ---\n");
        for d in 0..STATS_MAX_DEPTH {
            let total_cut = self.cutoff_by_depth[d];
            let move_count_sum = self.move_count_sum_by_depth[d];
            if total_cut > 0 {
                let avg = move_count_sum as f64 / total_cut as f64;
                report.push_str(&format!(
                    "  depth {:>2}: {:>6.2} avg ({} cutoffs)\n",
                    d, avg, total_cut
                ));
            }
        }
        // LMR削減量のヒストグラム
        report.push_str("--- LMR Reduction Histogram (r/1024) ---\n");
        for (r, &count) in self.lmr_reduction_histogram.iter().enumerate() {
            if count > 0 {
                let label = if r == 15 {
                    "15+".to_string()
                } else {
                    format!("{:>2}", r)
                };
                report.push_str(&format!(
                    "  r={}: {:>8} ({:>5.1}%)\n",
                    label,
                    count,
                    count as f64 / self.lmr_applied as f64 * 100.0
                ));
            }
        }
        // LMR適用後の新深度別ノード数
        report.push_str("--- LMR New Depth Distribution ---\n");
        for d in 0..STATS_MAX_DEPTH {
            let count = self.lmr_new_depth_histogram[d];
            if count > 0 {
                report.push_str(&format!(
                    "  new_depth {:>2}: {:>8} ({:>5.1}%)\n",
                    d,
                    count,
                    count as f64 / self.lmr_applied as f64 * 100.0
                ));
            }
        }
        report
    }
}

// =============================================================================
// 統計マクロ
// =============================================================================

/// 統計カウンタをインクリメントするマクロ（feature有効時のみ実行）
/// SearchWorker から呼び出される（self.state.stats へアクセス）
#[cfg(feature = "search-stats")]
macro_rules! inc_stat {
    ($self:expr, $field:ident) => {
        $self.state.stats.$field += 1;
    };
}

#[cfg(not(feature = "search-stats"))]
macro_rules! inc_stat {
    ($self:expr, $field:ident) => {};
}

/// 深度別統計をカウントするマクロ（feature有効時のみ実行）
/// SearchWorker から呼び出される（self.state.stats へアクセス）
#[cfg(feature = "search-stats")]
macro_rules! inc_stat_by_depth {
    ($self:expr, $field:ident, $depth:expr) => {
        let d = ($depth as usize).min($crate::search::stats::STATS_MAX_DEPTH - 1);
        $self.state.stats.$field[d] += 1;
    };
}

#[cfg(not(feature = "search-stats"))]
macro_rules! inc_stat_by_depth {
    ($self:expr, $field:ident, $depth:expr) => {};
}

// マクロを search モジュール内で使えるようにする
pub(super) use inc_stat;
pub(super) use inc_stat_by_depth;
