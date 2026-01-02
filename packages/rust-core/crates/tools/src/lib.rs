//! 将棋エンジン性能ベンチマークツール
//!
//! YaneuraOu の bench コマンド相当の標準ベンチマーク機能を提供します。
//!
//! ## 機能
//! - **内部APIモード**: Rust の Search API を直接呼び出してベンチマーク
//! - **USIモード**: 外部エンジンバイナリをUSIプロトコル経由で測定
//! - **複数スレッド対応**: スレッド数別のスケーリング測定
//! - **並列効率計算**: 理想的なスケーリングとの比較
//! - **Floodgate棋譜取得**: Floodgateサーバーから棋譜をダウンロード・変換
//!
//! ## 使用例
//!
//! ```rust,no_run
//! use tools::{BenchmarkConfig, EvalConfig, LimitType, runner};
//! use std::path::PathBuf;
//!
//! let config = BenchmarkConfig {
//!     threads: vec![1, 2, 4],
//!     tt_mb: 1024,
//!     limit_type: LimitType::Depth,
//!     limit: 10,
//!     sfens: None,
//!     iterations: 1,
//!     verbose: false,
//!     eval_config: EvalConfig::default(),
//!     reuse_search: false,
//!     warmup: 0,
//! };
//!
//! // 内部APIモード
//! let report = runner::internal::run_internal_benchmark(&config).unwrap();
//! report.print_summary();
//!
//! // USIモード
//! let engine_path = PathBuf::from("./engine");
//! let report = runner::usi::run_usi_benchmark(&config, &engine_path).unwrap();
//! report.print_summary();
//! ```

pub mod common;
pub mod config;
pub mod nnue_trainer;
pub mod positions;
pub mod report;
pub mod runner;
pub mod system;
mod utils;

// 公開API
pub use config::{BenchmarkConfig, EvalConfig, LimitType};
pub use positions::{load_positions, DEFAULT_POSITIONS};
pub use report::{Aggregate, BenchResult, BenchmarkReport, EvalInfo, ThreadResult};
pub use system::{collect_system_info, SystemInfo};
