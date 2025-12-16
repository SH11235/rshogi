//! 将棋エンジンベンチマークツール
//!
//! YaneuraOu の bench コマンド相当の標準ベンチマークを提供します。

use std::path::PathBuf;

use anyhow::Result;
use chrono::Local;
use clap::{Parser, ValueEnum};

use tools::{runner, BenchmarkConfig, LimitType};

/// 将棋エンジン汎用ベンチマークツール
#[derive(Parser, Debug)]
#[command(
    name = "benchmark",
    version,
    about = "将棋エンジン汎用ベンチマークツール",
    long_about = "YaneuraOu の bench コマンド相当の標準ベンチマーク機能を提供します。"
)]
struct Cli {
    /// 測定するスレッド数（カンマ区切り、例: \"1,2,4\"）
    #[arg(long, default_value = "1", value_delimiter = ',')]
    threads: Vec<usize>,

    /// 置換表サイズ（MB）
    #[arg(long, default_value = "1024")]
    tt_mb: u32,

    /// 制限タイプ
    #[arg(long, default_value = "movetime", value_enum)]
    limit_type: CliLimitType,

    /// 制限値（depth/nodes/movetime の値）
    #[arg(long, default_value = "15000")]
    limit: u64,

    /// SFEN局面ファイル（未指定時はデフォルト局面）
    #[arg(long)]
    sfens: Option<PathBuf>,

    /// 反復回数
    #[arg(long, default_value = "1")]
    iterations: u32,

    /// 結果JSONの出力ディレクトリ（デフォルト: ./benchmark_results）
    #[arg(long, default_value = "./benchmark_results")]
    output_dir: PathBuf,

    /// 詳細なinfo行を標準出力に表示
    #[arg(long, short = 'v')]
    verbose: bool,

    /// エンジンバイナリのパス（未指定時は内部APIを使用）
    #[arg(long)]
    engine: Option<PathBuf>,

    /// 内部API直接呼び出しモード（デバッグ用）
    #[arg(long)]
    internal: bool,
}

/// CLI用の制限タイプ（clap ValueEnum対応）
#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliLimitType {
    Depth,
    Nodes,
    Movetime,
}

impl From<CliLimitType> for LimitType {
    fn from(cli_type: CliLimitType) -> Self {
        match cli_type {
            CliLimitType::Depth => LimitType::Depth,
            CliLimitType::Nodes => LimitType::Nodes,
            CliLimitType::Movetime => LimitType::Movetime,
        }
    }
}

impl Cli {
    /// CLIからBenchmarkConfigを作成
    fn to_config(&self) -> BenchmarkConfig {
        BenchmarkConfig {
            threads: self.threads.clone(),
            tt_mb: self.tt_mb,
            limit_type: self.limit_type.into(),
            limit: self.limit,
            sfens: self.sfens.clone(),
            iterations: self.iterations,
            verbose: self.verbose,
        }
    }
}

/// 自動生成されるファイル名を作成
/// 形式: YYYYMMDDhhmmss_enginename_threads.json
fn generate_output_filename(engine_name: &str, threads: &[usize]) -> String {
    let timestamp = Local::now().format("%Y%m%d%H%M%S");
    let threads_str = threads.iter().map(|t| t.to_string()).collect::<Vec<_>>().join("-");

    // ファイル名に使えない文字を除去（パスインジェクション対策含む）
    let safe_engine_name: String = engine_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // 空文字列または長すぎる場合の対策
    let safe_engine_name = if safe_engine_name.is_empty() {
        "unknown".to_string()
    } else if safe_engine_name.len() > 100 {
        safe_engine_name[..100].to_string()
    } else {
        safe_engine_name
    };

    format!("{timestamp}_{safe_engine_name}_{threads_str}.json")
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 実行モード判定（if let パターンで unwrap を回避）
    let (report, engine_name) = if cli.internal {
        // 明示的に内部APIモードを指定
        println!("Running internal API mode...");
        let report = runner::internal::run_internal_benchmark(&cli.to_config())?;
        (report, "internal".to_string())
    } else if let Some(engine_path) = &cli.engine {
        // USIモード
        println!("Running USI mode with engine: {}", engine_path.display());
        let report = runner::usi::run_usi_benchmark(&cli.to_config(), engine_path)?;
        let name = engine_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        (report, name)
    } else {
        // デフォルト: 内部APIモード
        println!("Running internal API mode...");
        let report = runner::internal::run_internal_benchmark(&cli.to_config())?;
        (report, "internal".to_string())
    };

    // 出力ディレクトリを作成（存在しない場合）
    if !cli.output_dir.exists() {
        std::fs::create_dir_all(&cli.output_dir)?;
    }

    // 結果を常にファイル出力
    let output_filename = generate_output_filename(&engine_name, &cli.threads);
    let output_path = cli.output_dir.join(&output_filename);
    report.save_json(&output_path)?;
    println!("\nResults saved to: {}", output_path.display());

    // コンソール出力
    if cli.verbose {
        report.print_detailed();
    } else {
        report.print_summary();
    }

    Ok(())
}
