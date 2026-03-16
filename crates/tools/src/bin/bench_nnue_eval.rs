//! NNUE評価関数のベンチマークツール
//!
//! 3バリアント階層構造に対応したベンチマーク。
//! 各ネットワークアーキテクチャの推論性能を測定する。
//!
//! ## progress8kpabs bucket 計算ベンチ
//!
//! `--ls-progress-coeff` を指定すると、bucket index 計算のマイクロベンチも実行:
//! ```bash
//! cargo run --release --bin bench_nnue_eval -- \
//!   --nnue-file <path> \
//!   --ls-progress-coeff <progress.bin>
//! ```

use std::hint::black_box;
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;

use rshogi_core::nnue::{
    NNUEEvaluator, NNUENetwork, SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
    compute_layer_stack_progress8kpabs_bucket_index,
};
use rshogi_core::position::Position;

/// NNUE評価ベンチマーク
#[derive(Parser, Debug)]
#[command(
    name = "bench_nnue_eval",
    version,
    about = "NNUE評価関数のベンチマーク"
)]
struct Cli {
    /// NNUEファイルのパス
    #[arg(long)]
    nnue_file: PathBuf,

    /// 反復回数（デフォルト: 50万回）
    #[arg(long, default_value = "500000")]
    iterations: u64,

    /// ウォームアップ回数（デフォルト: 1万回）
    #[arg(long, default_value = "10000")]
    warmup: u64,

    /// progress8kpabs 重みファイル（progress.bin）
    /// 指定時は bucket index 計算のマイクロベンチも実行
    #[arg(long)]
    ls_progress_coeff: Option<PathBuf>,
}

/// ベンチマーク用のテスト局面（SFEN形式）
const TEST_POSITIONS: &[&str] = &[
    // 初期局面
    "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
    // 中盤局面1（矢倉模様）
    "lnsg1gsnl/1r4kb1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 1",
    // 中盤局面2（居飛車vs振り飛車）
    "ln1gkg1nl/1rs3sb1/p1pppp1pp/1p4p2/9/2PP5/PPS1PPPPP/1BG4R1/LN2KGSNL w - 1",
    // 終盤局面（駒が減った局面）
    "4k4/9/9/9/9/9/9/9/4K4 b 2r2b4g4s4n4l18p 1",
    // 複雑な中盤（駒の配置が多い）
    "l3kgsnl/3r2gb1/p1np1p1pp/1pp1p1p2/9/2PP1P3/PPSPPBPPP/2G3SR1/LN2KG1NL w Pp 1",
];

/// ベンチマーク結果
struct BenchResult {
    /// アーキテクチャ名
    arch_name: String,
    /// refresh_accumulator の結果
    refresh_ns_per_op: f64,
    /// evaluate の結果
    eval_ns_per_op: f64,
    /// 合計（refresh + evaluate）
    total_ns_per_op: f64,
    /// 評価回数/秒
    evals_per_sec: f64,
}

impl BenchResult {
    fn print(&self) {
        println!("=== {} ===", self.arch_name);
        println!("  refresh_accumulator: {:.1} ns/op", self.refresh_ns_per_op);
        println!("  evaluate:            {:.1} ns/op", self.eval_ns_per_op);
        println!("  total (refresh+eval):{:.1} ns/op", self.total_ns_per_op);
        println!("  throughput:          {:.0} evals/sec", self.evals_per_sec);
        println!();
    }
}

/// NNUEEvaluator を使用したベンチマーク
fn bench_evaluator(
    evaluator: &mut NNUEEvaluator,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
    arch_name: &str,
) -> BenchResult {
    // ウォームアップ
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        evaluator.refresh(pos);
        black_box(evaluator.evaluate_only(pos));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        evaluator.refresh(pos);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    evaluator.refresh(&positions[0]);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(evaluator.evaluate_only(pos));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        evaluator.refresh(pos);
        black_box(evaluator.evaluate_only(pos));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name: arch_name.to_string(),
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// progress.bin を読み込み f64 → f32 に変換
fn load_progress_kpabs_weights(path: &PathBuf) -> Result<Box<[f32]>> {
    let bytes = std::fs::read(path)?;
    let expected = SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * size_of::<f64>();
    anyhow::ensure!(
        bytes.len() == expected,
        "progress.bin size mismatch: got {} bytes, expected {}",
        bytes.len(),
        expected
    );
    let weights: Vec<f32> = bytes
        .chunks_exact(size_of::<f64>())
        .map(|chunk| f64::from_le_bytes(chunk.try_into().unwrap()) as f32)
        .collect();
    Ok(weights.into_boxed_slice())
}

/// progress8kpabs bucket 計算のマイクロベンチマーク
fn bench_progress_bucket(positions: &[Position], weights: &[f32], warmup: u64, iterations: u64) {
    // ウォームアップ
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        black_box(compute_layer_stack_progress8kpabs_bucket_index(
            pos,
            pos.side_to_move(),
            weights,
        ));
    }

    // 計測
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(compute_layer_stack_progress8kpabs_bucket_index(
            pos,
            pos.side_to_move(),
            weights,
        ));
    }
    let duration = start.elapsed();

    let ns_per_op = duration.as_nanos() as f64 / iterations as f64;
    let ops_per_sec = 1_000_000_000.0 / ns_per_op;

    // 各局面の bucket 値を表示
    println!("=== progress8kpabs bucket ===");
    for (i, pos) in positions.iter().enumerate() {
        let bucket =
            compute_layer_stack_progress8kpabs_bucket_index(pos, pos.side_to_move(), weights);
        println!("  position[{i}]: bucket={bucket}");
    }
    println!("  {:.1} ns/op ({:.0} ops/sec)", ns_per_op, ops_per_sec);
    println!();
    println!("--- progress8kpabs JSON ---");
    println!(r#"{{"bucket_ns":{:.1},"bucket_ops_per_sec":{:.0}}}"#, ns_per_op, ops_per_sec);
    println!();
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // テスト局面をパース
    let positions: Vec<Position> = TEST_POSITIONS
        .iter()
        .map(|sfen| {
            let mut pos = Position::new();
            pos.set_sfen(sfen).expect("Invalid SFEN");
            pos
        })
        .collect();

    println!("Benchmark config: {} warmup, {} iterations", cli.warmup, cli.iterations);
    println!("Test positions: {}", positions.len());
    println!();

    // progress8kpabs bucket ベンチマーク
    if let Some(ref coeff_path) = cli.ls_progress_coeff {
        println!("Loading progress8kpabs weights: {}", coeff_path.display());
        let weights = load_progress_kpabs_weights(coeff_path)?;
        println!("  weights: {} elements", weights.len());
        println!();
        bench_progress_bucket(&positions, &weights, cli.warmup, cli.iterations);
    }

    println!("Loading NNUE file: {}", cli.nnue_file.display());
    let network = Arc::new(NNUENetwork::load(&cli.nnue_file)?);
    let arch_name = network.architecture_name();
    println!("Architecture: {arch_name}");
    println!();

    // NNUEEvaluator を作成してベンチマーク実行
    let mut evaluator = NNUEEvaluator::new_with_position(Arc::clone(&network), &positions[0]);
    let result = bench_evaluator(&mut evaluator, &positions, cli.warmup, cli.iterations, arch_name);

    result.print();

    // JSON形式でも出力（後処理用）
    println!("--- JSON ---");
    println!(
        r#"{{"arch":"{}","refresh_ns":{:.1},"eval_ns":{:.1},"total_ns":{:.1},"evals_per_sec":{:.0}}}"#,
        result.arch_name,
        result.refresh_ns_per_op,
        result.eval_ns_per_op,
        result.total_ns_per_op,
        result.evals_per_sec
    );

    Ok(())
}
