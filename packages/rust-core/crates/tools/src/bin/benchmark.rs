//! 将棋エンジンベンチマークツール
//!
//! YaneuraOu の bench コマンド相当の標準ベンチマークを提供します。

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::{Parser, ValueEnum};

use tools::{run_internal_benchmark, BenchmarkConfig, LimitType};

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

    /// JSON形式で結果を出力
    #[arg(long)]
    json: Option<PathBuf>,

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

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 実行モード判定
    let report = if cli.internal || cli.engine.is_none() {
        println!("Running internal API mode...");
        run_internal_benchmark(&cli.to_config())?
    } else {
        return Err(anyhow!(
            "USI mode not yet implemented. Use --internal flag or omit --engine option."
        ));
    };

    // 結果出力
    if let Some(json_path) = &cli.json {
        report.save_json(json_path)?;
        println!("\nResults saved to: {}", json_path.display());
    }

    if cli.verbose {
        report.print_detailed();
    } else {
        report.print_summary();
    }

    Ok(())
}
