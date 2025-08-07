//! Benchmark metrics and statistics
//!
//! Provides utilities for calculating and analyzing benchmark results

use super::parallel::ParallelBenchmarkResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Summary statistics for benchmark results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSummary {
    /// Thread configuration summaries
    pub thread_results: HashMap<usize, ThreadSummary>,
    /// Overall performance metrics
    pub overall_metrics: OverallMetrics,
    /// Timestamp of the benchmark
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Git commit hash (if available)
    pub git_commit: Option<String>,
}

/// Summary for a specific thread configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub thread_count: usize,
    pub avg_nps: u64,
    pub avg_speedup: f64,
    pub avg_efficiency: f64,
    pub avg_duplication_rate: f64,
    pub avg_stop_latency_ms: f64,
    pub avg_pv_match_rate: f64,
}

/// Overall performance metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverallMetrics {
    /// Best NPS achieved
    pub best_nps: u64,
    /// Best thread count for NPS
    pub best_thread_count: usize,
    /// Average efficiency across all thread counts
    pub avg_efficiency: f64,
    /// Whether performance targets were met
    pub targets_met: TargetStatus,
}

/// Status of performance targets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetStatus {
    /// NPS(4T) ≥ 2.4×
    pub nps_4t_target: bool,
    /// Dup% ≤ 35%
    pub duplication_target: bool,
    /// PV不一致 ≤ 3%
    pub pv_match_target: bool,
    /// Stop latency ≤ 5ms
    pub stop_latency_target: bool,
}

/// Calculate summary statistics from benchmark results
pub fn calculate_summary(results: &[ParallelBenchmarkResult]) -> BenchmarkSummary {
    let mut thread_results = HashMap::new();

    // Group results by thread count
    for result in results {
        let entry = thread_results.entry(result.thread_count).or_insert_with(|| ThreadSummary {
            thread_count: result.thread_count,
            avg_nps: 0,
            avg_speedup: 0.0,
            avg_efficiency: 0.0,
            avg_duplication_rate: 0.0,
            avg_stop_latency_ms: 0.0,
            avg_pv_match_rate: 0.0,
        });

        // For now, just use the single result (in future, average multiple runs)
        entry.avg_nps = result.nps;
        entry.avg_speedup = result.speedup;
        entry.avg_efficiency = result.efficiency;
        entry.avg_duplication_rate = result.duplication_rate;
        entry.avg_stop_latency_ms = result.stop_latency_ms;
        entry.avg_pv_match_rate = result.pv_match_rate;
    }

    // Calculate overall metrics
    let best_result = results.iter().max_by_key(|r| r.nps);

    let avg_efficiency = if results.is_empty() {
        0.0
    } else {
        results.iter().map(|r| r.efficiency).sum::<f64>() / results.len() as f64
    };

    // Check performance targets
    let four_thread_result = results.iter().find(|r| r.thread_count == 4);
    let targets_met = TargetStatus {
        nps_4t_target: four_thread_result.map(|r| r.speedup >= 2.4).unwrap_or(false),
        duplication_target: results.iter().all(|r| r.duplication_rate <= 35.0),
        pv_match_target: results
            .iter()
            .filter(|r| r.thread_count > 1)
            .all(|r| r.pv_match_rate >= 97.0),
        stop_latency_target: results.iter().all(|r| r.stop_latency_ms <= 5.0),
    };

    let overall_metrics = OverallMetrics {
        best_nps: best_result.map(|r| r.nps).unwrap_or(0),
        best_thread_count: best_result.map(|r| r.thread_count).unwrap_or(1),
        avg_efficiency,
        targets_met,
    };

    BenchmarkSummary {
        thread_results,
        overall_metrics,
        timestamp: chrono::Utc::now(),
        git_commit: get_git_commit(),
    }
}

/// Get current git commit hash
fn get_git_commit() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .env_clear() // Clear environment variables for security
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
}

/// Compare two benchmark summaries for regression detection
pub fn compare_benchmarks(
    baseline: &BenchmarkSummary,
    current: &BenchmarkSummary,
    tolerance_percent: f64,
) -> RegressionReport {
    let mut regressions = Vec::new();

    // Compare each thread configuration
    for (thread_count, current_summary) in &current.thread_results {
        if let Some(baseline_summary) = baseline.thread_results.get(thread_count) {
            // Check NPS regression
            let nps_change = (current_summary.avg_nps as f64 - baseline_summary.avg_nps as f64)
                / baseline_summary.avg_nps as f64
                * 100.0;

            if nps_change < -tolerance_percent {
                regressions.push(Regression {
                    metric: "NPS".to_string(),
                    thread_count: *thread_count,
                    baseline_value: baseline_summary.avg_nps as f64,
                    current_value: current_summary.avg_nps as f64,
                    change_percent: nps_change,
                });
            }

            // Check duplication rate increase
            let dup_change =
                current_summary.avg_duplication_rate - baseline_summary.avg_duplication_rate;
            if dup_change > tolerance_percent {
                regressions.push(Regression {
                    metric: "Duplication Rate".to_string(),
                    thread_count: *thread_count,
                    baseline_value: baseline_summary.avg_duplication_rate,
                    current_value: current_summary.avg_duplication_rate,
                    change_percent: dup_change,
                });
            }

            // Check stop latency increase
            let latency_change =
                current_summary.avg_stop_latency_ms - baseline_summary.avg_stop_latency_ms;
            if latency_change > 0.5 {
                // 0.5ms absolute threshold
                regressions.push(Regression {
                    metric: "Stop Latency".to_string(),
                    thread_count: *thread_count,
                    baseline_value: baseline_summary.avg_stop_latency_ms,
                    current_value: current_summary.avg_stop_latency_ms,
                    change_percent: (latency_change / baseline_summary.avg_stop_latency_ms) * 100.0,
                });
            }
        }
    }

    // Check if performance targets are still met
    let targets_degraded = check_target_degradation(
        &baseline.overall_metrics.targets_met,
        &current.overall_metrics.targets_met,
    );

    RegressionReport {
        has_regression: !regressions.is_empty() || !targets_degraded.is_empty(),
        regressions,
        targets_degraded,
        baseline_commit: baseline.git_commit.clone(),
        current_commit: current.git_commit.clone(),
    }
}

/// Regression detection report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionReport {
    pub has_regression: bool,
    pub regressions: Vec<Regression>,
    pub targets_degraded: Vec<String>,
    pub baseline_commit: Option<String>,
    pub current_commit: Option<String>,
}

/// Individual regression detail
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Regression {
    pub metric: String,
    pub thread_count: usize,
    pub baseline_value: f64,
    pub current_value: f64,
    pub change_percent: f64,
}

/// Check which performance targets have degraded
fn check_target_degradation(baseline: &TargetStatus, current: &TargetStatus) -> Vec<String> {
    let mut degraded = Vec::new();

    if baseline.nps_4t_target && !current.nps_4t_target {
        degraded.push("NPS(4T) ≥ 2.4× target no longer met".to_string());
    }

    if baseline.duplication_target && !current.duplication_target {
        degraded.push("Duplication ≤ 35% target no longer met".to_string());
    }

    if baseline.pv_match_target && !current.pv_match_target {
        degraded.push("PV match ≥ 97% target no longer met".to_string());
    }

    if baseline.stop_latency_target && !current.stop_latency_target {
        degraded.push("Stop latency ≤ 5ms target no longer met".to_string());
    }

    degraded
}

/// Format regression report for display
pub fn format_regression_report(report: &RegressionReport) -> String {
    let mut output = String::new();

    if !report.has_regression {
        output.push_str("✅ No performance regressions detected\n");
        return output;
    }

    output.push_str("❌ Performance regressions detected:\n\n");

    // Format individual regressions
    if !report.regressions.is_empty() {
        output.push_str("## Metric Regressions\n");
        for reg in &report.regressions {
            output.push_str(&format!(
                "- **{}** ({} threads): {:.1} → {:.1} ({:+.1}%)\n",
                reg.metric,
                reg.thread_count,
                reg.baseline_value,
                reg.current_value,
                reg.change_percent
            ));
        }
        output.push('\n');
    }

    // Format target degradations
    if !report.targets_degraded.is_empty() {
        output.push_str("## Performance Targets No Longer Met\n");
        for target in &report.targets_degraded {
            output.push_str(&format!("- {target}\n"));
        }
        output.push('\n');
    }

    // Add commit information
    if let (Some(baseline), Some(current)) = (&report.baseline_commit, &report.current_commit) {
        output.push_str(&format!("Comparing {baseline} (baseline) → {current} (current)\n"));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_summary() {
        let results = vec![
            ParallelBenchmarkResult {
                thread_count: 1,
                nps: 100000,
                speedup: 1.0,
                efficiency: 1.0,
                duplication_rate: 0.0,
                stop_latency_ms: 1.0,
                pv_match_rate: 100.0,
                nodes: 100000,
                elapsed: std::time::Duration::from_secs(1),
            },
            ParallelBenchmarkResult {
                thread_count: 2,
                nps: 180000,
                speedup: 1.8,
                efficiency: 0.9,
                duplication_rate: 10.0,
                stop_latency_ms: 1.5,
                pv_match_rate: 98.0,
                nodes: 180000,
                elapsed: std::time::Duration::from_secs(1),
            },
        ];

        let summary = calculate_summary(&results);

        assert_eq!(summary.thread_results.len(), 2);
        assert_eq!(summary.overall_metrics.best_nps, 180000);
        assert_eq!(summary.overall_metrics.best_thread_count, 2);
        assert_eq!(summary.overall_metrics.avg_efficiency, 0.95);
    }

    #[test]
    fn test_empty_results_handling() {
        let results = vec![];

        // Should not panic on empty results
        let summary = calculate_summary(&results);

        assert_eq!(summary.thread_results.len(), 0);
        assert_eq!(summary.overall_metrics.best_nps, 0);
        assert_eq!(summary.overall_metrics.best_thread_count, 1);
        assert_eq!(summary.overall_metrics.avg_efficiency, 0.0);

        // All targets should be met for empty results (all() returns true for empty iterator)
        assert!(summary.overall_metrics.targets_met.duplication_target);
        assert!(summary.overall_metrics.targets_met.pv_match_target);
        assert!(summary.overall_metrics.targets_met.stop_latency_target);
    }

    #[test]
    fn test_regression_detection() {
        let baseline_results = vec![ParallelBenchmarkResult {
            thread_count: 1,
            nps: 100000,
            speedup: 1.0,
            efficiency: 1.0,
            duplication_rate: 0.0,
            stop_latency_ms: 1.0,
            pv_match_rate: 100.0,
            nodes: 100000,
            elapsed: std::time::Duration::from_secs(1),
        }];

        let current_results = vec![ParallelBenchmarkResult {
            thread_count: 1,
            nps: 95000, // 5% regression
            speedup: 1.0,
            efficiency: 1.0,
            duplication_rate: 0.0,
            stop_latency_ms: 1.0,
            pv_match_rate: 100.0,
            nodes: 95000,
            elapsed: std::time::Duration::from_secs(1),
        }];

        let baseline_summary = calculate_summary(&baseline_results);
        let current_summary = calculate_summary(&current_results);

        let report = compare_benchmarks(&baseline_summary, &current_summary, 2.0);

        assert!(report.has_regression);
        assert_eq!(report.regressions.len(), 1);
        assert_eq!(report.regressions[0].metric, "NPS");
        assert_eq!(report.regressions[0].change_percent, -5.0);
    }
}
