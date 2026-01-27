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
//! # 詳細オプション（学習率スケジューリング、バリデーション）
//! cargo run -p tools --release --bin train_nnue -- \
//!   --input train.jsonl --output-dir models \
//!   --epochs 200 --batch-size 16384 \
//!   --eta1 0.001 --eta2 0.0001 --eta3 0.00001 \
//!   --eta1-epoch 100 --eta2-epoch 200 \
//!   --validation val.jsonl \
//!   --newbob-decay 0.5 --newbob-trials 3
//!
//! # 既存モデルからのリジューム
//! cargo run -p tools --release --bin train_nnue -- \
//!   --input train.jsonl --output-dir models \
//!   --resume models/nnue_epoch_50.bin --epochs 100
//! ```

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use tools::nnue_trainer::{Adam, LossType, TrainConfig, Trainer, TrainingDataset};

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

    /// 学習率（eta1と同義）
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

    // === 学習率スケジューリング ===
    /// 初期学習率（eta1）
    #[arg(long)]
    eta1: Option<f32>,

    /// 中間学習率（eta2）
    #[arg(long)]
    eta2: Option<f32>,

    /// 最終学習率（eta3）
    #[arg(long)]
    eta3: Option<f32>,

    /// eta1からeta2への遷移完了エポック
    #[arg(long, default_value_t = 0)]
    eta1_epoch: usize,

    /// eta2からeta3への遷移完了エポック
    #[arg(long, default_value_t = 0)]
    eta2_epoch: usize,

    // === Validationとリジューム ===
    /// 検証データファイル（JSONL形式）
    #[arg(long)]
    validation: Option<PathBuf>,

    /// 既存モデルからのリジューム
    #[arg(long)]
    resume: Option<PathBuf>,

    // === Newbobスケジューリング ===
    /// Newbob減衰率（1.0で無効）
    #[arg(long, default_value_t = 1.0)]
    newbob_decay: f32,

    /// Newbob最大試行回数
    #[arg(long, default_value_t = 2)]
    newbob_trials: usize,

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

    // 検証データの読み込み
    let validation = if let Some(ref val_path) = cli.validation {
        eprintln!("Loading validation data from {}", val_path.display());
        let val_dataset = TrainingDataset::load(val_path, None).with_context(|| {
            format!("Failed to load validation data from {}", val_path.display())
        })?;
        eprintln!("Loaded {} validation samples", val_dataset.len());
        Some(val_dataset)
    } else {
        None
    };

    // 学習設定
    let loss_type = match cli.loss {
        LossArg::Mse => LossType::Mse,
        LossArg::Sigmoid => LossType::SigmoidCrossEntropy,
    };

    // 学習率の設定
    let eta1 = cli.eta1.unwrap_or(cli.lr);
    let eta2 = cli.eta2.unwrap_or(eta1);
    let eta3 = cli.eta3.unwrap_or(eta2);

    let config = TrainConfig {
        batch_size: cli.batch_size,
        epochs: cli.epochs,
        learning_rate: cli.lr,
        weight_decay: cli.weight_decay,
        seed: cli.seed,
        loss_type,
        checkpoint_interval: cli.checkpoint,
        output_dir: cli.output_dir.to_string_lossy().to_string(),
        eta1,
        eta2,
        eta3,
        eta1_epoch: cli.eta1_epoch,
        eta2_epoch: cli.eta2_epoch,
        newbob_decay: cli.newbob_decay,
        newbob_num_trials: cli.newbob_trials,
        resume_path: cli.resume.map(|p| p.to_string_lossy().to_string()),
        ..Default::default()
    };

    // トレーナーの作成
    let mut trainer = Trainer::new(config).context("Failed to create trainer")?;

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
    trainer.train(&mut dataset, validation.as_ref(), &mut optimizer);

    eprintln!("\nTraining complete!");
    Ok(())
}
