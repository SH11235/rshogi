//! dlshogi 系 ONNX の policy で種の局面から指し進め、途中の局面を PSV に書き出す。
//!
//! 1 手ごとに policy を 1 回推論し、合法手の確率から温度付きで抽選する (探索はしない)。
//! 多数の指し進めを同時に走らせ、全局面を 1 回の batch 推論にまとめる。同じ推論の
//! value head を各局面の score (手番視点 cp) にする。
//!
//! 入力ファイルごとに出力ディレクトリを分け、種を `--chunk-seeds` 件ずつの chunk に分けて
//! chunk ごとに 1 ファイルを書く。完成した chunk は再実行時に飛ばすので、中断しても続きから再開できる。

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tools::policy_rollout::{GameResultMode, RolloutConfig};

#[derive(Parser, Debug)]
#[command(
    name = "policy_rollout_psv",
    version,
    about = "dlshogi 系 ONNX の policy で種の局面から指し進め、途中の局面を PSV に書き出す\n\n\
             score は同じ推論の value head (手番視点 cp)。探索はしない。"
)]
struct Cli {
    /// 種の局面の PSV。複数指定すると指定順に 1 ファイルずつ全量を処理する
    #[arg(short, long, required = true, num_args = 1..)]
    input: Vec<PathBuf>,

    /// 出力ディレクトリ (入力ファイル名から拡張子を除いた名前の下に、chunk ごとの `rollout-NNNNNN.psv` を書く)
    #[arg(short, long)]
    output_dir: PathBuf,

    /// 標準 dlshogi ONNX モデル (DL水匠等)
    #[arg(long)]
    dlshogi_onnx_model: PathBuf,

    /// 1 つの種から始める指し進めの本数
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    rollouts_per_seed: u32,

    /// 同時に進める指し進めの数 (= 1 回の推論の最大 batch)
    #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u64).range(1..))]
    parallel: u64,

    /// 1 chunk (= 1 出力ファイル、再開の単位) に入れる種の数
    #[arg(long, default_value_t = 10_000, value_parser = clap::value_parser!(u64).range(1..))]
    chunk_seeds: u64,

    /// 抽選の温度。確率 p の手を p^(1/T) で重み付けする。0 で常に最善手
    #[arg(long, default_value_t = 1.0)]
    temperature: f32,

    /// 確率 (温度をかける前、合法手で正規化) がこれ未満の手を抽選から外す。最善手は常に残す
    #[arg(long, default_value_t = 0.01)]
    min_prob: f32,

    /// 確率の上位 k 手だけから抽選する (0 で制限なし)
    #[arg(long, default_value_t = 0)]
    top_k: usize,

    /// 通し手数 (game_ply) がこの値に達したら打ち切る
    #[arg(long, default_value_t = 512, value_parser = clap::value_parser!(u16).range(1..))]
    max_game_ply: u16,

    /// 出力の game_result に入れる値
    #[arg(long, value_enum, default_value_t = GameResultMode::Zero)]
    game_result: GameResultMode,

    /// 乱数の seed (入力と option が同じなら出力は bit 一致する)
    #[arg(long, default_value_t = 1)]
    seed: u64,

    /// 種からの最初の 1 手で、種のレコードの move16 (元の対局で指された手) も抽選の対象にする。
    /// 未指定ならその手を除く (move16 が 0 か非合法なら何もしない)
    #[arg(long)]
    allow_seed_move: bool,

    /// CUDA device id (負値で CPU 推論)
    #[arg(long, default_value_t = 0)]
    onnx_gpu_id: i32,

    /// TensorRT EP (FP16) を使う。未指定なら CUDA EP (FP32)
    #[arg(long)]
    onnx_tensorrt: bool,

    /// TensorRT エンジンキャッシュの保存先 (`--onnx-tensorrt` 時のみ有効)
    #[arg(long)]
    onnx_tensorrt_cache: Option<PathBuf>,

    /// winrate→cp 変換スケール
    #[arg(long, default_value_t = 600.0)]
    onnx_eval_scale: f32,
}

/// 入力ファイルごとの出力ディレクトリと、乱数列を分ける識別子 (ファイル名の FNV-1a)。
///
/// 識別子をファイル名から作るので、入力を渡す順を変えても各ファイルの出力は変わらない。
fn input_streams(cli: &Cli) -> Result<Vec<(PathBuf, PathBuf, u64)>> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::with_capacity(cli.input.len());
    for input in &cli.input {
        let stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("input has no UTF-8 file name: {}", input.display()))?;
        if !seen.insert(stem.to_owned()) {
            anyhow::bail!("two inputs share the file name '{stem}'; rename one of them");
        }
        let stream = stem.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        out.push((input.clone(), cli.output_dir.join(stem), stream));
    }
    Ok(out)
}

fn validate(cli: &Cli) -> Result<RolloutConfig> {
    if !cli.temperature.is_finite() || cli.temperature < 0.0 {
        anyhow::bail!("--temperature must be >= 0, got {}", cli.temperature);
    }
    if !(0.0..=1.0).contains(&cli.min_prob) {
        anyhow::bail!("--min-prob must be in [0, 1], got {}", cli.min_prob);
    }
    if !cli.onnx_eval_scale.is_finite() || cli.onnx_eval_scale <= 0.0 {
        anyhow::bail!("--onnx-eval-scale must be a positive finite value");
    }
    if cli.onnx_tensorrt && cli.onnx_gpu_id < 0 {
        anyhow::bail!("--onnx-tensorrt requires a GPU (--onnx-gpu-id >= 0)");
    }
    Ok(RolloutConfig {
        rollouts_per_seed: cli.rollouts_per_seed,
        parallel: cli.parallel as usize,
        temperature: cli.temperature,
        min_prob: cli.min_prob,
        top_k: cli.top_k,
        max_game_ply: cli.max_game_ply,
        game_result: cli.game_result,
        seed: cli.seed,
        avoid_seed_move: !cli.allow_seed_move,
    })
}

#[cfg(not(feature = "dlshogi-onnx"))]
fn main() -> Result<()> {
    let cli = Cli::parse();
    validate(&cli)?;
    input_streams(&cli)?;
    anyhow::bail!(
        "policy_rollout_psv requires the 'dlshogi-onnx' feature (on by default; this build disabled it).\n\
         Rebuild with default features: cargo build --release -p tools --bin policy_rollout_psv"
    )
}

#[cfg(feature = "dlshogi-onnx")]
fn main() -> Result<()> {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tools::onnx_value::{OnnxValueConfig, OnnxValueEvaluator};
    use tools::output_path::ensure_safe_output_paths;
    use tools::policy_rollout::{chunk_file_name, run_all};

    static INTERRUPTED: AtomicBool = AtomicBool::new(false);

    let cli = Cli::parse();
    let cfg = validate(&cli)?;
    let streams = input_streams(&cli)?;

    let ort_library = std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from);
    let mut readonly: Vec<&Path> = cli.input.iter().map(PathBuf::as_path).collect();
    readonly.push(cli.dlshogi_onnx_model.as_path());
    if let Some(lib) = ort_library.as_deref().filter(|p| p.is_file()) {
        readonly.push(lib);
    }
    // 出力ディレクトリ自体が資源と衝突しないことを、推論器の初期化前に確かめる。
    for (_, dir, _) in &streams {
        std::fs::create_dir_all(dir)?;
        ensure_safe_output_paths(&[&dir.join(chunk_file_name(0))], &readonly)?;
    }

    let mut model = OnnxValueEvaluator::new(&OnnxValueConfig {
        model_path: cli.dlshogi_onnx_model.clone(),
        gpu_id: cli.onnx_gpu_id,
        use_tensorrt: cli.onnx_tensorrt,
        tensorrt_cache: cli.onnx_tensorrt_cache.clone(),
        eval_scale: cli.onnx_eval_scale,
        batch_size: cfg.parallel,
    })?;

    if let Err(e) = ctrlc::set_handler(|| {
        INTERRUPTED.store(true, Ordering::SeqCst);
        eprintln!("\nInterrupted. The current chunk is discarded; rerun to resume.");
    }) {
        eprintln!("WARNING: failed to set Ctrl+C handler: {e}");
    }

    for (input, dir, stream) in &streams {
        run_all(&mut model, input, *stream, dir, cli.chunk_seeds, &cfg, &readonly, &INTERRUPTED)?;
    }
    tools::ort_teardown::exit_skipping_ort_teardown(0)
}
