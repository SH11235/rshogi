//! NNUE学習ツール
//!
//! 教師データからNNUEモデルを学習する。
//!
//! # 使用例
//!
//! ```bash
//! # 基本的な学習
//! cargo run -p tools --release --bin train_nnue -- \
//!   --input train.jsonl --output-dir models --epochs 100
//!
//! # 詳細オプション
//! cargo run -p tools --release --bin train_nnue -- \
//!   --input train.jsonl --output-dir models \
//!   --epochs 100 --batch-size 16384 --lr 0.001 \
//!   --loss sigmoid --checkpoint 10
//! ```

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use tools::nnue_trainer::{Adam, TrainConfig, Trainer, TrainingDataset};

#[derive(Parser)]
#[command(
    name = "train-nnue",
    version,
    about = "NNUE学習ツール\n\n教師データからNNUEモデルを学習する"
)]
struct Cli {
    /// 入力ファイル（JSONL形式の教師データ）
    #[arg(short, long)]
    input: PathBuf,

    /// 出力ディレクトリ
    #[arg(short, long, default_value = ".")]
    output_dir: PathBuf,

    /// エポック数
    #[arg(long, default_value_t = 100)]
    epochs: usize,

    /// バッチサイズ
    #[arg(long, default_value_t = 16384)]
    batch_size: usize,

    /// 学習率
    #[arg(long, default_value_t = 0.001)]
    lr: f32,

    /// 重み減衰
    #[arg(long, default_value_t = 0.0)]
    weight_decay: f32,

    /// 損失関数
    #[arg(long, value_enum, default_value_t = LossArg::Mse)]
    loss: LossArg,

    /// チェックポイント保存間隔（エポック単位）
    #[arg(long, default_value_t = 10)]
    checkpoint: usize,

    /// シード値
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// 読み込むサンプル数の上限（0=無制限）
    #[arg(long, default_value_t = 0)]
    limit: usize,

    /// 詳細出力
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum LossArg {
    /// 平均二乗誤差
    Mse,
    /// シグモイド交差エントロピー
    Sigmoid,
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    // 入力ファイルの存在確認
    if !cli.input.exists() {
        anyhow::bail!("Input file not found: {}", cli.input.display());
    }

    // 出力ディレクトリの作成
    if !cli.output_dir.exists() {
        std::fs::create_dir_all(&cli.output_dir).with_context(|| {
            format!("Failed to create output directory: {}", cli.output_dir.display())
        })?;
    }

    // 教師データの読み込み
    eprintln!("Loading training data from {}", cli.input.display());
    let limit = if cli.limit > 0 { Some(cli.limit) } else { None };
    let mut dataset = TrainingDataset::load(&cli.input, limit)
        .with_context(|| format!("Failed to load training data from {}", cli.input.display()))?;

    if dataset.is_empty() {
        anyhow::bail!("No training samples loaded");
    }

    eprintln!("Loaded {} training samples", dataset.len());

    // 学習設定
    let loss_type = match cli.loss {
        LossArg::Mse => tools::nnue_trainer::trainer::LossType::Mse,
        LossArg::Sigmoid => tools::nnue_trainer::trainer::LossType::SigmoidCrossEntropy,
    };

    let config = TrainConfig {
        batch_size: cli.batch_size,
        epochs: cli.epochs,
        learning_rate: cli.lr,
        weight_decay: cli.weight_decay,
        seed: cli.seed,
        loss_type,
        checkpoint_interval: cli.checkpoint,
        output_dir: cli.output_dir.to_string_lossy().to_string(),
        ..Default::default()
    };

    // トレーナーの作成
    let mut trainer = Trainer::new(config);

    // Ctrl-Cハンドラの設定
    let interrupted = trainer.interrupted();
    ctrlc::set_handler(move || {
        eprintln!("\nInterrupted, finishing current batch...");
        interrupted.store(true, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;

    // オプティマイザの作成
    let mut optimizer = Adam::new(trainer.network(), cli.lr).with_weight_decay(cli.weight_decay);

    // 学習の実行
    eprintln!("\nStarting training...");
    trainer.train(&mut dataset, &mut optimizer);

    eprintln!("\nTraining complete!");
    Ok(())
}
