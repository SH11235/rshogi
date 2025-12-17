//! ベンチマーク結果の型定義と出力機能

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::EvalConfig;
use crate::system::SystemInfo;
use crate::utils::format_number;

/// 評価関数情報（JSON出力用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalInfo {
    /// NNUE評価が有効かどうか
    /// - `true`: NNUEファイルが指定され、NNUE評価を使用
    /// - `false`: Material評価のみを使用
    pub nnue_enabled: bool,
    /// 使用されたNNUEファイルのパス
    /// NNUE評価が無効の場合は `None`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_file: Option<String>,
    /// Material評価レベル（1, 2, 3, 4, 7, 8, 9）
    /// レベルが高いほど、より詳細な評価を行う（ただしNPSは低下する）
    pub material_level: u8,
}

impl From<&EvalConfig> for EvalInfo {
    fn from(config: &EvalConfig) -> Self {
        EvalInfo {
            nnue_enabled: config.nnue_file.is_some(),
            nnue_file: config.nnue_file.as_ref().map(|p| p.display().to_string()),
            material_level: config.material_level,
        }
    }
}

/// 単一局面のベンチマーク結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchResult {
    /// 局面の SFEN 文字列
    pub sfen: String,
    /// 到達した探索深さ
    pub depth: i32,
    /// 探索したノード数
    pub nodes: u64,
    /// 探索時間（ミリ秒）
    pub time_ms: u64,
    /// ノード毎秒（Nodes Per Second）
    pub nps: u64,
    /// 置換表使用率（パーミル: 0-1000）
    pub hashfull: u32,
    /// 最善手（USI 形式）
    pub bestmove: String,
    /// ウォームアップ実行かどうか（reuse_searchモード時のみ設定）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_warmup: Option<bool>,
    /// Search再利用モードでの探索実行インデックス（0=初回、1=2回目...）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_run_index: Option<u32>,
}

/// スレッド数別の結果
///
/// 特定のスレッド数で実行した全局面の結果をまとめて保持します。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadResult {
    /// 使用したスレッド数
    pub threads: usize,
    /// 各局面のベンチマーク結果
    pub results: Vec<BenchResult>,
}

/// 集計統計
///
/// [`ThreadResult`] の結果を集計した統計情報です。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Aggregate {
    /// 合計ノード数
    pub total_nodes: u64,
    /// 合計探索時間（ミリ秒）
    pub total_time_ms: u64,
    /// 平均 NPS（合計ノード / 合計時間から算出）
    pub average_nps: u64,
    /// 平均探索深さ
    pub average_depth: f64,
    /// 平均置換表使用率
    pub average_hashfull: f64,
}

impl ThreadResult {
    /// 結果を集計
    pub fn aggregate(&self) -> Aggregate {
        if self.results.is_empty() {
            return Aggregate {
                total_nodes: 0,
                total_time_ms: 0,
                average_nps: 0,
                average_depth: 0.0,
                average_hashfull: 0.0,
            };
        }

        let total_nodes: u64 = self.results.iter().map(|r| r.nodes).sum();
        let total_time_ms: u64 = self.results.iter().map(|r| r.time_ms).sum();
        let average_nps = if total_time_ms > 0 {
            (total_nodes as f64 * 1000.0 / total_time_ms as f64) as u64
        } else {
            0
        };

        let count = self.results.len() as f64;
        let average_depth = self.results.iter().map(|r| r.depth as f64).sum::<f64>() / count;
        let average_hashfull = self.results.iter().map(|r| r.hashfull as f64).sum::<f64>() / count;

        Aggregate {
            total_nodes,
            total_time_ms,
            average_nps,
            average_depth,
            average_hashfull,
        }
    }
}

/// ベンチマークレポート
///
/// 全ベンチマーク結果をまとめたトップレベル構造体です。
/// JSON ファイルへのシリアライズ/デシリアライズに対応しています。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    /// システム情報（CPU、OS など）
    pub system_info: SystemInfo,
    /// エンジン名（USI モード時のみ設定）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_name: Option<String>,
    /// エンジンの実行パス（USI モード時のみ設定）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_path: Option<String>,
    /// 評価関数情報
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_info: Option<EvalInfo>,
    /// スレッド数別の結果リスト
    pub results: Vec<ThreadResult>,
}

impl BenchmarkReport {
    /// 人間可読な形式で結果を出力
    pub fn print_summary(&self) {
        println!("\n=== Benchmark Summary ===");
        if let Some(name) = &self.engine_name {
            println!("Engine: {name}");
        }
        if let Some(path) = &self.engine_path {
            println!("Engine Path: {path}");
        }
        println!("CPU: {}", self.system_info.cpu_model);
        println!("Cores: {}", self.system_info.cpu_cores);
        println!("OS: {}", self.system_info.os);
        println!("Date: {}\n", self.system_info.timestamp);

        // ベースラインNPS（1スレッド目）を取得
        let baseline_nps = self.results.first().map(|r| r.aggregate().average_nps).unwrap_or(0);

        // スレッド数が2つ以上の場合は効率列も表示
        let show_efficiency = self.results.len() > 1;

        if show_efficiency {
            println!(
                "{:<10} {:<15} {:<15} {:<15} {:<10}",
                "Threads", "Total Nodes", "Total Time", "Avg NPS", "Efficiency"
            );
            println!("{}", "-".repeat(70));
        } else {
            println!(
                "{:<10} {:<15} {:<15} {:<15}",
                "Threads", "Total Nodes", "Total Time", "Avg NPS"
            );
            println!("{}", "-".repeat(55));
        }

        for thread_result in &self.results {
            let agg = thread_result.aggregate();
            let efficiency =
                calculate_efficiency(baseline_nps, agg.average_nps, thread_result.threads);

            if show_efficiency {
                println!(
                    "{:<10} {:<15} {:<15} {:<15} {:<9.1}%",
                    thread_result.threads,
                    format_number(agg.total_nodes),
                    format!("{}ms", agg.total_time_ms),
                    format_number(agg.average_nps),
                    efficiency,
                );
            } else {
                println!(
                    "{:<10} {:<15} {:<15} {:<15}",
                    thread_result.threads,
                    format_number(agg.total_nodes),
                    format!("{}ms", agg.total_time_ms),
                    format_number(agg.average_nps),
                );
            }
        }

        println!();
    }

    /// 詳細レポートを出力
    pub fn print_detailed(&self) {
        self.print_summary();

        println!("=== Detailed Results ===\n");

        for thread_result in &self.results {
            println!("--- Threads: {} ---", thread_result.threads);

            for (idx, result) in thread_result.results.iter().enumerate() {
                println!("  Position {}:", idx + 1);
                println!("    SFEN: {}", result.sfen);
                println!("    Depth: {}", result.depth);
                println!("    Nodes: {}", format_number(result.nodes));
                println!("    Time: {}ms", result.time_ms);
                println!("    NPS: {}", format_number(result.nps));
                println!("    Hashfull: {}", result.hashfull);
                println!("    Bestmove: {}", result.bestmove);
            }
            println!();
        }
    }

    /// JSON形式で保存
    pub fn save_json(&self, path: &Path) -> Result<()> {
        let file = File::create(path)
            .with_context(|| format!("Failed to create JSON file: {}", path.display()))?;
        serde_json::to_writer_pretty(file, self).with_context(|| "Failed to write JSON")?;
        Ok(())
    }
}

/// 並列効率を計算
///
/// # Arguments
/// * `baseline_nps` - ベースライン（1スレッド）のNPS
/// * `current_nps` - 現在のNPS
/// * `threads` - スレッド数
///
/// # Returns
/// 並列効率（%）。理想的なスケーリングは100%。
pub fn calculate_efficiency(baseline_nps: u64, current_nps: u64, threads: usize) -> f64 {
    if baseline_nps == 0 || threads == 0 {
        return 0.0;
    }
    let speedup = current_nps as f64 / baseline_nps as f64;
    (speedup / threads as f64) * 100.0
}

/// reuse_searchモード専用の集計統計
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReuseSearchStats {
    /// 初回探索（Search::new()直後）の平均NPS
    pub first_run_avg_nps: u64,
    /// 2回目以降の平均NPS（履歴蓄積後）
    pub subsequent_avg_nps: u64,
    /// NPS改善率（%）
    pub improvement_percent: f64,
    /// 局面ごとの詳細
    pub per_position: Vec<PositionReuseStats>,
}

/// 局面ごとのreuse_search統計
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionReuseStats {
    /// 局面名（SFENの先頭部分）
    pub position_name: String,
    /// 初回NPS
    pub first_nps: u64,
    /// 2回目以降の平均NPS
    pub subsequent_avg_nps: u64,
    /// NPS改善率（%）
    pub improvement_percent: f64,
}

impl BenchmarkReport {
    /// reuse_searchモードの統計を計算
    pub fn compute_reuse_stats(&self) -> Option<ReuseSearchStats> {
        // search_run_indexが設定されている結果のみを対象
        let thread_result = self.results.first()?;
        let results: Vec<_> = thread_result
            .results
            .iter()
            .filter(|r| r.search_run_index.is_some() && r.is_warmup != Some(true))
            .collect();

        if results.is_empty() {
            return None;
        }

        // SFENごとにグループ化
        let mut sfen_groups: std::collections::HashMap<String, Vec<&BenchResult>> =
            std::collections::HashMap::new();
        for r in &results {
            sfen_groups.entry(r.sfen.clone()).or_default().push(r);
        }

        // 各SFENの初回と2回目以降を分離
        let mut first_runs = Vec::new();
        let mut subsequent_runs = Vec::new();
        let mut per_position = Vec::new();

        for (sfen, group) in &sfen_groups {
            // search_run_indexでソート
            let mut sorted: Vec<_> = group.iter().collect();
            sorted.sort_by_key(|r| r.search_run_index.unwrap_or(0));

            if let Some(first) = sorted.first() {
                first_runs.push(first.nps);

                let subsequent: Vec<u64> = sorted.iter().skip(1).map(|r| r.nps).collect();
                if !subsequent.is_empty() {
                    let subseq_avg = subsequent.iter().sum::<u64>() / subsequent.len() as u64;
                    subsequent_runs.extend(subsequent.iter());

                    let improvement = if first.nps > 0 {
                        ((subseq_avg as f64 - first.nps as f64) / first.nps as f64) * 100.0
                    } else {
                        0.0
                    };

                    per_position.push(PositionReuseStats {
                        position_name: truncate_sfen(sfen),
                        first_nps: first.nps,
                        subsequent_avg_nps: subseq_avg,
                        improvement_percent: improvement,
                    });
                }
            }
        }

        if first_runs.is_empty() {
            return None;
        }

        let first_avg = first_runs.iter().sum::<u64>() / first_runs.len() as u64;
        let subsequent_avg = if subsequent_runs.is_empty() {
            first_avg
        } else {
            subsequent_runs.iter().sum::<u64>() / subsequent_runs.len() as u64
        };

        let improvement = if first_avg > 0 {
            ((subsequent_avg as f64 - first_avg as f64) / first_avg as f64) * 100.0
        } else {
            0.0
        };

        Some(ReuseSearchStats {
            first_run_avg_nps: first_avg,
            subsequent_avg_nps: subsequent_avg,
            improvement_percent: improvement,
            per_position,
        })
    }

    /// reuse_searchモード用のサマリーを出力
    pub fn print_reuse_summary(&self) {
        if let Some(stats) = self.compute_reuse_stats() {
            println!("\n=== Reuse Search Analysis ===");
            println!(
                "First run (cold start): avg NPS = {}",
                format_number(stats.first_run_avg_nps)
            );
            println!(
                "Subsequent runs:        avg NPS = {}",
                format_number(stats.subsequent_avg_nps)
            );
            println!("NPS Improvement:        {:+.1}%", stats.improvement_percent);

            if !stats.per_position.is_empty() {
                println!("\nPosition-by-position breakdown:");
                println!(
                    "  {:<20} | {:<12} | {:<12} | Improvement",
                    "Position", "First NPS", "Subseq NPS"
                );
                println!("  {}", "-".repeat(60));

                for pos in &stats.per_position {
                    println!(
                        "  {:<20} | {:<12} | {:<12} | {:+.1}%",
                        pos.position_name,
                        format_number(pos.first_nps),
                        format_number(pos.subsequent_avg_nps),
                        pos.improvement_percent
                    );
                }
            }
            println!();
        }
    }
}

/// SFENを短く表示用にトランケート
fn truncate_sfen(sfen: &str) -> String {
    if sfen.len() <= 20 {
        sfen.to_string()
    } else {
        format!("{}...", &sfen[..17])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aggregate_empty() {
        let thread_result = ThreadResult {
            threads: 1,
            results: vec![],
        };
        let agg = thread_result.aggregate();
        assert_eq!(agg.total_nodes, 0);
        assert_eq!(agg.average_nps, 0);
    }

    #[test]
    fn test_calculate_efficiency() {
        // 理想的なスケーリング（効率100%）
        assert_eq!(calculate_efficiency(100_000, 200_000, 2), 100.0);
        assert_eq!(calculate_efficiency(100_000, 400_000, 4), 100.0);

        // 非理想的なスケーリング（効率75%）
        assert_eq!(calculate_efficiency(100_000, 300_000, 4), 75.0);

        // エッジケース
        assert_eq!(calculate_efficiency(0, 100_000, 2), 0.0);
        assert_eq!(calculate_efficiency(100_000, 0, 0), 0.0);
    }
}
