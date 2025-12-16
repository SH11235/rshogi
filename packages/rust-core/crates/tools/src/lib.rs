//! 将棋エンジン性能ベンチマークツール
//!
//! YaneuraOu の bench コマンド相当の標準ベンチマーク機能を提供します。

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::thread;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sysinfo::System;

use engine_core::position::Position;
use engine_core::search::{init_search_module, LimitsType, Search, SearchInfo};

// =============================================================================
// 定数定義
// =============================================================================

/// 探索スレッドのスタックサイズ（64MB）
/// engine-usiと同じ値を使用
const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

// =============================================================================
// デフォルト局面セット
// =============================================================================

/// YaneuraOu準拠のデフォルトベンチマーク局面
/// memo/YaneuraOu/source/benchmark.cpp の Defaults から引用
pub const DEFAULT_POSITIONS: &[(&str, &str)] = &[
    // 1. 初期局面に近い局面
    (
        "hirate-like",
        "lnsgkgsnl/1r7/p1ppp1bpp/1p3pp2/7P1/2P6/PP1PPPP1P/1B3S1R1/LNSGKG1NL b - 9",
    ),
    // 2. 読めば読むほど後手悪いような局面
    (
        "complex-middle",
        "l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1",
    ),
    // 3. 57同銀は詰み、みたいな。読めば読むほど先手が悪いことがわかってくる局面
    (
        "tactical",
        "6n1l/2+S1k4/2lp4p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 b GSN2L4Pgs2p 1",
    ),
    // 4. 指し手生成祭りの局面
    // cf. http://d.hatena.ne.jp/ak11/20110508/p1
    (
        "movegen-heavy",
        "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
    ),
];

// =============================================================================
// 構造体定義
// =============================================================================

/// 制限タイプ
#[derive(Debug, Clone, Copy)]
pub enum LimitType {
    Depth,
    Nodes,
    Movetime,
}

impl LimitType {
    pub fn to_usi_cmd(self) -> &'static str {
        match self {
            LimitType::Depth => "depth",
            LimitType::Nodes => "nodes",
            LimitType::Movetime => "movetime",
        }
    }
}

/// 単一局面のベンチマーク結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchResult {
    pub sfen: String,
    pub depth: i32,
    pub nodes: u64,
    pub time_ms: u64,
    pub nps: u64,
    pub hashfull: u32,
    pub bestmove: String,
}

/// スレッド数別の結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadResult {
    pub threads: usize,
    pub results: Vec<BenchResult>,
}

/// 集計統計
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Aggregate {
    pub total_nodes: u64,
    pub total_time_ms: u64,
    pub average_nps: u64,
    pub average_depth: f64,
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

/// システム情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub timestamp: String,
    pub cpu_model: String,
    pub cpu_cores: usize,
    pub os: String,
    pub arch: String,
}

/// ベンチマークレポート
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub system_info: SystemInfo,
    pub results: Vec<ThreadResult>,
}

/// ベンチマーク設定
#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    pub threads: Vec<usize>,
    pub tt_mb: u32,
    pub limit_type: LimitType,
    pub limit: u64,
    pub sfens: Option<PathBuf>,
    pub iterations: u32,
    pub verbose: bool,
}

// =============================================================================
// システム情報収集
// =============================================================================

/// システム情報を収集
pub fn collect_system_info() -> SystemInfo {
    let mut sys = System::new_all();
    sys.refresh_cpu_all();

    let cpu_model = sys.cpus().first().map(|cpu| cpu.brand()).unwrap_or("Unknown").to_string();

    SystemInfo {
        timestamp: chrono::Utc::now().to_rfc3339(),
        cpu_model,
        cpu_cores: sys.cpus().len(),
        os: System::name().unwrap_or_else(|| "Unknown".to_string()),
        arch: std::env::consts::ARCH.to_string(),
    }
}

// =============================================================================
// 局面ロード
// =============================================================================

/// 局面を読み込む
pub fn load_positions(config: &BenchmarkConfig) -> Result<Vec<(String, String)>> {
    if let Some(path) = &config.sfens {
        load_positions_from_file(path)
    } else {
        Ok(DEFAULT_POSITIONS
            .iter()
            .map(|(name, sfen)| (name.to_string(), sfen.to_string()))
            .collect())
    }
}

/// SFEN局面ファイルを読み込む
fn load_positions_from_file(path: &Path) -> Result<Vec<(String, String)>> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open positions file: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut positions = Vec::new();

    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        let line = line.trim();

        // コメント行と空行をスキップ
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // "name | sfen" 形式をパース
        if let Some((name, sfen)) = line.split_once('|') {
            positions.push((name.trim().to_string(), sfen.trim().to_string()));
        } else {
            // 区切り文字がない場合は、インデックスを名前として使用
            positions.push((format!("position_{}", idx + 1), line.to_string()));
        }
    }

    if positions.is_empty() {
        anyhow::bail!("No positions found in file: {}", path.display());
    }

    Ok(positions)
}

// =============================================================================
// 内部APIベンチマーク実装
// =============================================================================

/// 内部API直接呼び出しモードでベンチマークを実行
pub fn run_internal_benchmark(config: &BenchmarkConfig) -> Result<BenchmarkReport> {
    // 探索モジュール初期化
    init_search_module();

    let positions = load_positions(config)?;
    let mut all_results = Vec::new();

    for threads in &config.threads {
        if *threads > 1 {
            eprintln!("Warning: Multi-threading not yet implemented in internal API mode");
            eprintln!("         Running with single thread instead.");
        }

        println!("=== Threads: {} ===", threads);

        let mut thread_results = Vec::new();
        let tt_mb = config.tt_mb;

        for iteration in 0..config.iterations {
            if config.iterations > 1 {
                println!("Iteration {}/{}", iteration + 1, config.iterations);
            }

            for (name, sfen) in &positions {
                if config.verbose {
                    println!("  Position: {name}");
                }

                // 局面設定
                let mut pos = Position::new();
                pos.set_sfen(sfen).with_context(|| format!("Invalid SFEN: {sfen}"))?;

                // 制限設定
                let mut limits = LimitsType::default();
                limits.set_start_time();

                match config.limit_type {
                    LimitType::Depth => limits.depth = config.limit as i32,
                    LimitType::Nodes => limits.nodes = config.limit,
                    LimitType::Movetime => limits.movetime = config.limit as i64,
                }

                // 探索実行（専用スタックサイズのスレッドで実行）
                // 各局面ごとに新しいSearchオブジェクトを作成
                let verbose = config.verbose;
                let sfen_clone = sfen.to_string();
                let bench_result = thread::Builder::new()
                    .stack_size(SEARCH_STACK_SIZE)
                    .spawn(move || {
                        // スレッド内でSearchエンジンを作成
                        let mut search = Search::new(tt_mb as usize);

                        let mut last_info: Option<SearchInfo> = None;
                        let result = search.go(
                            &mut pos,
                            limits,
                            Some(|info: &SearchInfo| {
                                last_info = Some(info.clone());
                                if verbose {
                                    println!("    {}", info.to_usi_string());
                                }
                            }),
                        );

                        // 結果収集
                        BenchResult {
                            sfen: sfen_clone,
                            depth: result.depth,
                            nodes: result.nodes,
                            time_ms: last_info.as_ref().map(|i| i.time_ms).unwrap_or(0),
                            nps: last_info.as_ref().map(|i| i.nps).unwrap_or(0),
                            hashfull: last_info.as_ref().map(|i| i.hashfull).unwrap_or(0),
                            bestmove: result.best_move.to_usi(),
                        }
                    })
                    .with_context(|| "Failed to spawn search thread")?
                    .join()
                    .map_err(|_| anyhow::anyhow!("Search thread panicked"))?;

                if config.verbose {
                    println!(
                        "    depth={} nodes={} time={}ms nps={}",
                        bench_result.depth,
                        bench_result.nodes,
                        bench_result.time_ms,
                        bench_result.nps
                    );
                }

                thread_results.push(bench_result);
            }
        }

        all_results.push(ThreadResult {
            threads: *threads,
            results: thread_results,
        });
    }

    Ok(BenchmarkReport {
        system_info: collect_system_info(),
        results: all_results,
    })
}

// =============================================================================
// 結果出力
// =============================================================================

impl BenchmarkReport {
    /// 人間可読な形式で結果を出力
    pub fn print_summary(&self) {
        println!("\n=== Benchmark Summary ===");
        println!("CPU: {}", self.system_info.cpu_model);
        println!("Cores: {}", self.system_info.cpu_cores);
        println!("OS: {}", self.system_info.os);
        println!("Date: {}\n", self.system_info.timestamp);

        println!("{:<10} {:<15} {:<15} {:<15}", "Threads", "Total Nodes", "Total Time", "Avg NPS");
        println!("{}", "-".repeat(55));

        for thread_result in &self.results {
            let agg = thread_result.aggregate();
            println!(
                "{:<10} {:<15} {:<15} {:<15}",
                thread_result.threads,
                format_number(agg.total_nodes),
                format!("{}ms", agg.total_time_ms),
                format_number(agg.average_nps),
            );
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

// =============================================================================
// ヘルパー関数
// =============================================================================

/// 数値を3桁区切りでフォーマット
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut result = String::new();

    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(b as char);
    }

    result
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_positions() {
        assert!(!DEFAULT_POSITIONS.is_empty());
        assert_eq!(DEFAULT_POSITIONS.len(), 4); // YaneuraOu準拠で4局面

        for (name, sfen) in DEFAULT_POSITIONS {
            assert!(!name.is_empty());
            assert!(!sfen.is_empty());
        }
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(1234567), "1,234,567");
        assert_eq!(format_number(123), "123");
        assert_eq!(format_number(0), "0");
    }

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
}
