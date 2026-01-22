//! NNUE評価関数のベンチマークツール
//!
//! const-generics実装前のデグレ検知用ベンチマーク。
//! 各ネットワークアーキテクチャの推論性能を測定する。

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Result};
use clap::Parser;

use engine_core::nnue::{
    Accumulator, AccumulatorHalfKA, AccumulatorHalfKADynamic, AccumulatorHalfKP,
    AccumulatorHalfKPDynamic, HalfKA1024CReLU, HalfKA1024SCReLU, HalfKA512CReLU, HalfKA512SCReLU,
    HalfKP256CReLU, HalfKP256SCReLU, HalfKP512CReLU, HalfKP512SCReLU, NNUENetwork,
};
use engine_core::position::Position;

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

    /// 反復回数（デフォルト: 100万回）
    #[arg(long, default_value = "1000000")]
    iterations: u64,

    /// ウォームアップ回数（デフォルト: 1万回）
    #[arg(long, default_value = "10000")]
    warmup: u64,

    /// HalfKP 256x2-32-32 の場合、元の静的実装と const generics 実装を両方比較
    #[arg(long, default_value = "false")]
    compare: bool,
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

/// HalfKP (256x2-32-32) のベンチマーク
fn bench_halfkp(
    network: &engine_core::nnue::Network,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    // ウォームアップ
    let mut acc = Accumulator::default();
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.feature_transformer.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.feature_transformer.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    // まずAccumulatorを設定
    network.feature_transformer.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク（refresh + evaluate）
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.feature_transformer.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name: "HalfKP 256x2-32-32 (static)".to_string(),
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// HalfKP256 (256x2-32-32) CReLU のベンチマーク (const generics)
fn bench_halfkp256_crelu(
    network: &HalfKP256CReLU,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    // ウォームアップ
    let mut acc = AccumulatorHalfKP::<256>::default();
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    network.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name: "HalfKP256 256x2-32-32 CReLU (const generics)".to_string(),
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// HalfKP256 (256x2-32-32) SCReLU のベンチマーク (const generics)
fn bench_halfkp256_screlu(
    network: &HalfKP256SCReLU,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    // ウォームアップ
    let mut acc = AccumulatorHalfKP::<256>::default();
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    network.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name: "HalfKP256 256x2-32-32 SCReLU (const generics)".to_string(),
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// HalfKP512 (512x2-8-96) CReLU のベンチマーク (const generics)
fn bench_halfkp512_crelu(
    network: &HalfKP512CReLU,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    // ウォームアップ
    let mut acc = AccumulatorHalfKP::<512>::default();
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    network.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name: "HalfKP512 512x2-8-96 CReLU (const generics)".to_string(),
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// HalfKP512 (512x2-8-96) SCReLU のベンチマーク (const generics)
fn bench_halfkp512_screlu(
    network: &HalfKP512SCReLU,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    // ウォームアップ
    let mut acc = AccumulatorHalfKP::<512>::default();
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    network.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name: "HalfKP512 512x2-8-96 SCReLU (const generics)".to_string(),
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// HalfKA512 (512x2-8-96) CReLU のベンチマーク
fn bench_halfka512_crelu(
    network: &HalfKA512CReLU,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    // ウォームアップ
    let mut acc = AccumulatorHalfKA::<512>::default();
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    network.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name: "HalfKA512 512x2-8-96 CReLU (const generics)".to_string(),
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// HalfKA512 (512x2-8-96) SCReLU のベンチマーク
fn bench_halfka512_screlu(
    network: &HalfKA512SCReLU,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    // ウォームアップ
    let mut acc = AccumulatorHalfKA::<512>::default();
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    network.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name: "HalfKA512 512x2-8-96 SCReLU (const generics)".to_string(),
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// HalfKA1024 (1024x2-8-96) CReLU のベンチマーク
fn bench_halfka1024_crelu(
    network: &HalfKA1024CReLU,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    // ウォームアップ
    let mut acc = AccumulatorHalfKA::<1024>::default();
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    network.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name: "HalfKA1024 1024x2-8-96 CReLU (const generics)".to_string(),
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// HalfKA1024 (1024x2-8-96) SCReLU のベンチマーク
fn bench_halfka1024_screlu(
    network: &HalfKA1024SCReLU,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    // ウォームアップ
    let mut acc = AccumulatorHalfKA::<1024>::default();
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    network.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name: "HalfKA1024 1024x2-8-96 SCReLU (const generics)".to_string(),
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// HalfKADynamic のベンチマーク
fn bench_halfka_dynamic(
    network: &engine_core::nnue::NetworkHalfKADynamic,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    let arch_name = format!(
        "HalfKADynamic {}x2-{}-{} (dynamic)",
        network.arch_l1, network.arch_l2, network.arch_l3
    );

    // ウォームアップ
    let mut acc = AccumulatorHalfKADynamic::new(network.arch_l1);
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    network.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name,
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
}

/// HalfKPDynamic のベンチマーク
fn bench_halfkp_dynamic(
    network: &engine_core::nnue::NetworkHalfKPDynamic,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> BenchResult {
    let arch_name = format!(
        "HalfKPDynamic {}x2-{}-{} (dynamic)",
        network.arch_l1, network.arch_l2, network.arch_l3
    );

    // ウォームアップ
    let mut acc = AccumulatorHalfKPDynamic::new(network.arch_l1);
    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        network.feature_transformer.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }

    // refresh_accumulator ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.feature_transformer.refresh_accumulator(pos, &mut acc);
    }
    let refresh_duration = start.elapsed();

    // evaluate ベンチマーク
    network.feature_transformer.refresh_accumulator(&positions[0], &mut acc);
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        black_box(network.evaluate(pos, &acc));
    }
    let eval_duration = start.elapsed();

    // 結合ベンチマーク
    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        network.feature_transformer.refresh_accumulator(pos, &mut acc);
        black_box(network.evaluate(pos, &acc));
    }
    let total_duration = start.elapsed();

    let refresh_ns = refresh_duration.as_nanos() as f64 / iterations as f64;
    let eval_ns = eval_duration.as_nanos() as f64 / iterations as f64;
    let total_ns = total_duration.as_nanos() as f64 / iterations as f64;

    BenchResult {
        arch_name,
        refresh_ns_per_op: refresh_ns,
        eval_ns_per_op: eval_ns,
        total_ns_per_op: total_ns,
        evals_per_sec: 1_000_000_000.0 / total_ns,
    }
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

    // --compare モード: 元の静的実装と const generics 実装を両方ベンチマーク
    if cli.compare {
        use engine_core::nnue::{HalfKP256CReLU, Network};
        use std::fs::File;
        use std::io::BufReader;

        println!("=== Compare Mode: Static vs Const Generics ===\n");

        // 元の静的実装を読み込み
        println!("Loading static implementation (Network)...");
        let file = File::open(&cli.nnue_file)?;
        let mut reader = BufReader::new(file);
        let static_network = Network::read(&mut reader)?;
        let result_static = bench_halfkp(&static_network, &positions, cli.warmup, cli.iterations);
        result_static.print();

        // const generics 実装を読み込み
        println!("Loading const generics implementation (HalfKP256CReLU)...");
        let file = File::open(&cli.nnue_file)?;
        let mut reader = BufReader::new(file);
        let cg_network = HalfKP256CReLU::read(&mut reader)?;
        let result_cg = bench_halfkp256_crelu(&cg_network, &positions, cli.warmup, cli.iterations);
        result_cg.print();

        // 比較結果
        println!("=== Comparison ===");
        let refresh_diff = (result_cg.refresh_ns_per_op - result_static.refresh_ns_per_op)
            / result_static.refresh_ns_per_op
            * 100.0;
        let eval_diff = (result_cg.eval_ns_per_op - result_static.eval_ns_per_op)
            / result_static.eval_ns_per_op
            * 100.0;
        let total_diff = (result_cg.total_ns_per_op - result_static.total_ns_per_op)
            / result_static.total_ns_per_op
            * 100.0;
        println!(
            "  refresh: {:.1} ns (static) vs {:.1} ns (cg) = {:+.1}%",
            result_static.refresh_ns_per_op, result_cg.refresh_ns_per_op, refresh_diff
        );
        println!(
            "  evaluate: {:.1} ns (static) vs {:.1} ns (cg) = {:+.1}%",
            result_static.eval_ns_per_op, result_cg.eval_ns_per_op, eval_diff
        );
        println!(
            "  total: {:.1} ns (static) vs {:.1} ns (cg) = {:+.1}%",
            result_static.total_ns_per_op, result_cg.total_ns_per_op, total_diff
        );

        return Ok(());
    }

    // 通常モード
    println!("Loading NNUE file: {}", cli.nnue_file.display());
    let network = NNUENetwork::load(&cli.nnue_file)?;
    let arch_name = network.architecture_name();
    println!("Architecture: {}", arch_name);
    println!();

    // アーキテクチャに応じてベンチマーク実行
    let result = match network {
        NNUENetwork::HalfKP(net) => bench_halfkp(&net, &positions, cli.warmup, cli.iterations),
        NNUENetwork::HalfKP256CReLU(net) => {
            bench_halfkp256_crelu(&net, &positions, cli.warmup, cli.iterations)
        }
        NNUENetwork::HalfKP256SCReLU(net) => {
            bench_halfkp256_screlu(&net, &positions, cli.warmup, cli.iterations)
        }
        NNUENetwork::HalfKP512CReLU(net) => {
            bench_halfkp512_crelu(&net, &positions, cli.warmup, cli.iterations)
        }
        NNUENetwork::HalfKP512SCReLU(net) => {
            bench_halfkp512_screlu(&net, &positions, cli.warmup, cli.iterations)
        }
        NNUENetwork::HalfKPDynamic(net) => {
            bench_halfkp_dynamic(&net, &positions, cli.warmup, cli.iterations)
        }
        NNUENetwork::HalfKA512CReLU(net) => {
            bench_halfka512_crelu(&net, &positions, cli.warmup, cli.iterations)
        }
        NNUENetwork::HalfKA512SCReLU(net) => {
            bench_halfka512_screlu(&net, &positions, cli.warmup, cli.iterations)
        }
        NNUENetwork::HalfKA1024CReLU(net) => {
            bench_halfka1024_crelu(&net, &positions, cli.warmup, cli.iterations)
        }
        NNUENetwork::HalfKA1024SCReLU(net) => {
            bench_halfka1024_screlu(&net, &positions, cli.warmup, cli.iterations)
        }
        NNUENetwork::HalfKADynamic(net) => {
            bench_halfka_dynamic(&net, &positions, cli.warmup, cli.iterations)
        }
        NNUENetwork::LayerStacks(_) => {
            bail!("LayerStacks benchmark not implemented yet");
        }
    };

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
