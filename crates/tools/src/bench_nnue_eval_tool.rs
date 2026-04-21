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

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;

use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::nnue::{
    AccumulatorCacheLayerStacks, AccumulatorLayerStacks, DirtyPiece, LayerStackBucketMode,
    LayerStacksNetwork, NNUEEvaluator, NNUENetwork, NetworkLayerStacks,
    SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, compute_layer_stack_progress8kpabs_bucket_index,
    get_layer_stack_progress_kpabs_weights, parse_layer_stack_bucket_mode,
    set_layer_stack_bucket_mode, set_layer_stack_progress_kpabs_weights,
    sqr_clipped_relu_transform,
};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, PieceType};

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

    /// ベンチモード
    #[arg(long, default_value = "full")]
    mode: String,

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

    /// LayerStacks bucket モード
    #[arg(long, default_value = "progress8kpabs")]
    ls_bucket_mode: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchMode {
    Full,
    LayerStackPropagate,
    LayerStackEval,
    LayerStackRefreshCache,
    LayerStackUpdateCache,
}

impl BenchMode {
    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full" => Ok(Self::Full),
            "layer-stack-propagate" => Ok(Self::LayerStackPropagate),
            "layer-stack-eval" => Ok(Self::LayerStackEval),
            "layer-stack-refresh-cache" => Ok(Self::LayerStackRefreshCache),
            "layer-stack-update-cache" => Ok(Self::LayerStackUpdateCache),
            _ => bail!(
                "unknown --mode '{}'. expected one of: full, layer-stack-propagate, layer-stack-eval, layer-stack-refresh-cache, layer-stack-update-cache",
                value
            ),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::LayerStackPropagate => "layer-stack-propagate",
            Self::LayerStackEval => "layer-stack-eval",
            Self::LayerStackRefreshCache => "layer-stack-refresh-cache",
            Self::LayerStackUpdateCache => "layer-stack-update-cache",
        }
    }
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
    "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
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

struct LayerStackBenchResult {
    bench_name: &'static str,
    ns_per_op: f64,
    ops_per_sec: f64,
}

impl LayerStackBenchResult {
    fn print(
        &self,
        arch_name: &str,
        bucket_mode: LayerStackBucketMode,
        bucket_counts: &[usize; 9],
    ) {
        println!("=== {arch_name} / {} ===", self.bench_name);
        println!("  bucket_mode:         {}", bucket_mode.as_str());
        println!("  dataset buckets:     {}", format_bucket_counts(bucket_counts));
        println!("  ns/op:               {:.1}", self.ns_per_op);
        println!("  throughput:          {:.0} ops/sec", self.ops_per_sec);
        println!();
    }
}

#[derive(Clone)]
struct LayerStackPropagateCase<const L1: usize> {
    bucket_index: usize,
    transformed: [u8; L1],
}

#[derive(Clone)]
struct LayerStackEvalCase<const L1: usize> {
    pos: Position,
    bucket_index: usize,
    accumulator: AccumulatorLayerStacks<L1>,
}

#[derive(Clone)]
struct LayerStackUpdateCacheCase<const L1: usize> {
    pos: Position,
    dirty_piece: DirtyPiece,
    prev_accumulator: AccumulatorLayerStacks<L1>,
}

struct LayerStackCases<const L1: usize> {
    propagate_cases: Vec<LayerStackPropagateCase<L1>>,
    eval_cases: Vec<LayerStackEvalCase<L1>>,
    update_cache_cases: Vec<LayerStackUpdateCacheCase<L1>>,
    bucket_counts: [usize; 9],
    update_bucket_counts: [usize; 9],
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

fn compute_layer_stack_bucket_index(pos: &Position, mode: LayerStackBucketMode) -> usize {
    let side_to_move = pos.side_to_move();
    match mode {
        LayerStackBucketMode::Progress8KPAbs => compute_layer_stack_progress8kpabs_bucket_index(
            pos,
            side_to_move,
            get_layer_stack_progress_kpabs_weights(),
        ),
    }
}

fn format_bucket_counts(bucket_counts: &[usize; 9]) -> String {
    let parts: Vec<String> = bucket_counts
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(bucket, count)| format!("{bucket}:{count}"))
        .collect();
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(", ")
    }
}

fn configure_layer_stack_bucket(
    cli: &Cli,
    progress_weights: Option<&[f32]>,
) -> Result<LayerStackBucketMode> {
    let mode = parse_layer_stack_bucket_mode(&cli.ls_bucket_mode).ok_or_else(|| {
        anyhow!("invalid --ls-bucket-mode '{}'. expected: progress8kpabs", cli.ls_bucket_mode)
    })?;
    set_layer_stack_bucket_mode(mode);

    if mode == LayerStackBucketMode::Progress8KPAbs {
        let weights = progress_weights.context(
            "--ls-bucket-mode progress8kpabs requires --ls-progress-coeff <progress.bin>",
        )?;
        set_layer_stack_progress_kpabs_weights(weights.to_vec().into_boxed_slice())
            .map_err(|e| anyhow!("failed to set progress8kpabs weights: {e}"))?;
    }

    Ok(mode)
}

fn prepare_layer_stack_cases<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
>(
    net: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    positions: &[Position],
    bucket_mode: LayerStackBucketMode,
) -> Result<LayerStackCases<L1>> {
    let mut propagate_cases = Vec::with_capacity(positions.len());
    let mut eval_cases = Vec::with_capacity(positions.len());
    let mut update_cache_cases = Vec::with_capacity(positions.len());
    let mut bucket_counts = [0usize; 9];
    let mut update_bucket_counts = [0usize; 9];

    for pos in positions {
        let mut accumulator = AccumulatorLayerStacks::<L1>::new();
        net.refresh_accumulator(pos, &mut accumulator);

        let bucket_index = compute_layer_stack_bucket_index(pos, bucket_mode);
        bucket_counts[bucket_index] += 1;

        let (us_acc, them_acc) = if pos.side_to_move() == Color::Black {
            (accumulator.get(Color::Black as usize), accumulator.get(Color::White as usize))
        } else {
            (accumulator.get(Color::White as usize), accumulator.get(Color::Black as usize))
        };

        let mut transformed = [0u8; L1];
        sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed);

        propagate_cases.push(LayerStackPropagateCase {
            bucket_index,
            transformed,
        });
        eval_cases.push(LayerStackEvalCase {
            pos: pos.clone(),
            bucket_index,
            accumulator: accumulator.clone(),
        });

        if let Some(mv) = select_non_king_legal_move(pos) {
            let mut next_pos = pos.clone();
            let dirty_piece = next_pos.do_move(mv, pos.gives_check(mv));
            let next_bucket_index = compute_layer_stack_bucket_index(&next_pos, bucket_mode);
            update_bucket_counts[next_bucket_index] += 1;

            update_cache_cases.push(LayerStackUpdateCacheCase {
                pos: next_pos,
                dirty_piece,
                prev_accumulator: accumulator,
            });
        }
    }

    Ok(LayerStackCases {
        propagate_cases,
        eval_cases,
        update_cache_cases,
        bucket_counts,
        update_bucket_counts,
    })
}

fn select_non_king_legal_move(pos: &Position) -> Option<rshogi_core::types::Move> {
    let mut moves = MoveList::new();
    generate_legal_all(pos, &mut moves);
    moves
        .iter()
        .copied()
        .find(|mv| mv.is_drop() || pos.piece_on(mv.from()).piece_type() != PieceType::King)
}

fn bench_layer_stack_propagate<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
>(
    net: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    cases: &[LayerStackPropagateCase<L1>],
    warmup: u64,
    iterations: u64,
) -> LayerStackBenchResult {
    for i in 0..warmup {
        let case = &cases[i as usize % cases.len()];
        black_box(net.layer_stacks.buckets[case.bucket_index].propagate(&case.transformed));
    }

    let start = Instant::now();
    for i in 0..iterations {
        let case = &cases[i as usize % cases.len()];
        black_box(net.layer_stacks.buckets[case.bucket_index].propagate(&case.transformed));
    }
    let duration = start.elapsed();

    let ns_per_op = duration.as_nanos() as f64 / iterations as f64;
    LayerStackBenchResult {
        bench_name: "layer_stack_propagate",
        ns_per_op,
        ops_per_sec: 1_000_000_000.0 / ns_per_op,
    }
}

fn bench_layer_stack_eval<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
>(
    net: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    cases: &[LayerStackEvalCase<L1>],
    warmup: u64,
    iterations: u64,
) -> LayerStackBenchResult {
    for i in 0..warmup {
        let case = &cases[i as usize % cases.len()];
        black_box(net.evaluate_with_bucket(&case.pos, &case.accumulator, case.bucket_index));
    }

    let start = Instant::now();
    for i in 0..iterations {
        let case = &cases[i as usize % cases.len()];
        black_box(net.evaluate_with_bucket(&case.pos, &case.accumulator, case.bucket_index));
    }
    let duration = start.elapsed();

    let ns_per_op = duration.as_nanos() as f64 / iterations as f64;
    LayerStackBenchResult {
        bench_name: "layer_stack_eval",
        ns_per_op,
        ops_per_sec: 1_000_000_000.0 / ns_per_op,
    }
}

fn bench_layer_stack_refresh_cache<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
>(
    net: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    positions: &[Position],
    warmup: u64,
    iterations: u64,
) -> LayerStackBenchResult {
    let mut cache = AccumulatorCacheLayerStacks::<L1>::new();
    let mut accumulator = AccumulatorLayerStacks::<L1>::new();

    for i in 0..warmup {
        let pos = &positions[i as usize % positions.len()];
        net.refresh_accumulator_with_cache(pos, &mut accumulator, &mut cache);
        black_box(accumulator.get(0)[0]);
    }

    let start = Instant::now();
    for i in 0..iterations {
        let pos = &positions[i as usize % positions.len()];
        net.refresh_accumulator_with_cache(pos, &mut accumulator, &mut cache);
        black_box(accumulator.get(0)[0]);
    }
    let duration = start.elapsed();

    let ns_per_op = duration.as_nanos() as f64 / iterations as f64;
    LayerStackBenchResult {
        bench_name: "layer_stack_refresh_cache",
        ns_per_op,
        ops_per_sec: 1_000_000_000.0 / ns_per_op,
    }
}

fn bench_layer_stack_update_cache<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
>(
    net: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    cases: &[LayerStackUpdateCacheCase<L1>],
    warmup: u64,
    iterations: u64,
) -> Result<LayerStackBenchResult> {
    if cases.is_empty() {
        bail!("LayerStack update-cache ベンチ用の非玉合法手付き局面がない");
    }

    let mut cache = AccumulatorCacheLayerStacks::<L1>::new();
    let mut accumulator = AccumulatorLayerStacks::<L1>::new();

    for i in 0..warmup {
        let case = &cases[i as usize % cases.len()];
        net.update_accumulator_with_cache(
            &case.pos,
            &case.dirty_piece,
            &mut accumulator,
            &case.prev_accumulator,
            &mut cache,
        );
        black_box(accumulator.get(0)[0]);
    }

    let start = Instant::now();
    for i in 0..iterations {
        let case = &cases[i as usize % cases.len()];
        net.update_accumulator_with_cache(
            &case.pos,
            &case.dirty_piece,
            &mut accumulator,
            &case.prev_accumulator,
            &mut cache,
        );
        black_box(accumulator.get(0)[0]);
    }
    let duration = start.elapsed();

    let ns_per_op = duration.as_nanos() as f64 / iterations as f64;
    Ok(LayerStackBenchResult {
        bench_name: "layer_stack_update_cache",
        ns_per_op,
        ops_per_sec: 1_000_000_000.0 / ns_per_op,
    })
}

fn print_ls_json(
    mode: BenchMode,
    arch_name: &str,
    bucket_mode: LayerStackBucketMode,
    bucket_counts: &[usize; 9],
    result: &LayerStackBenchResult,
) {
    println!("--- JSON ---");
    println!(
        r#"{{"mode":"{}","arch":"{}","bucket_mode":"{}","bucket_counts":"{}","ns_per_op":{:.1},"ops_per_sec":{:.0}}}"#,
        mode.as_str(),
        arch_name,
        bucket_mode.as_str(),
        format_bucket_counts(bucket_counts),
        result.ns_per_op,
        result.ops_per_sec
    );
}

/// LayerStack ベンチマークの共通実行ロジック
fn run_layer_stack_bench<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
>(
    net: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    mode: BenchMode,
    positions: &[Position],
    bucket_mode: LayerStackBucketMode,
    warmup: u64,
    iterations: u64,
    arch_name: &str,
) -> Result<()> {
    let cases = prepare_layer_stack_cases(net, positions, bucket_mode)?;

    let (result, bucket_counts) = match mode {
        BenchMode::LayerStackPropagate => (
            bench_layer_stack_propagate(net, &cases.propagate_cases, warmup, iterations),
            &cases.bucket_counts,
        ),
        BenchMode::LayerStackEval => (
            bench_layer_stack_eval(net, &cases.eval_cases, warmup, iterations),
            &cases.bucket_counts,
        ),
        BenchMode::LayerStackRefreshCache => (
            bench_layer_stack_refresh_cache(net, positions, warmup, iterations),
            &cases.bucket_counts,
        ),
        BenchMode::LayerStackUpdateCache => {
            let result =
                bench_layer_stack_update_cache(net, &cases.update_cache_cases, warmup, iterations)?;
            result.print(arch_name, bucket_mode, &cases.update_bucket_counts);
            print_ls_json(mode, arch_name, bucket_mode, &cases.update_bucket_counts, &result);
            return Ok(());
        }
        BenchMode::Full => unreachable!(),
    };

    result.print(arch_name, bucket_mode, bucket_counts);
    print_ls_json(mode, arch_name, bucket_mode, bucket_counts, &result);
    Ok(())
}

/// LayerStacksNetwork の L1 dispatch マクロ
macro_rules! with_ls_net {
    ($ls_net:expr, |$inner:ident| $body:expr) => {
        match $ls_net {
            #[cfg(feature = "layerstacks-1536x16x32")]
            LayerStacksNetwork::L1536x16x32($inner) => $body,
            #[cfg(feature = "layerstacks-1536x32x32")]
            LayerStacksNetwork::L1536x32x32($inner) => $body,
            #[cfg(feature = "layerstacks-768x16x32")]
            LayerStacksNetwork::L768($inner) => $body,
            #[allow(unreachable_patterns)]
            _ => bail!("有効な LayerStacks バリアントがありません"),
        }
    };
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let mode = BenchMode::parse(&cli.mode)?;

    // テスト局面をパース
    let positions: Vec<Position> = TEST_POSITIONS
        .iter()
        .map(|sfen| {
            let mut pos = Position::new();
            pos.set_sfen(sfen).expect("Invalid SFEN");
            pos
        })
        .collect();

    println!(
        "Benchmark config: mode={}, {} warmup, {} iterations",
        mode.as_str(),
        cli.warmup,
        cli.iterations
    );
    println!("Test positions: {}", positions.len());
    println!();

    let progress_weights = if let Some(ref coeff_path) = cli.ls_progress_coeff {
        println!("Loading progress8kpabs weights: {}", coeff_path.display());
        let weights = load_progress_kpabs_weights(coeff_path)?;
        println!("  weights: {} elements", weights.len());
        println!();
        Some(weights)
    } else {
        None
    };

    // progress8kpabs bucket ベンチマーク
    if let Some(weights) = progress_weights.as_deref() {
        bench_progress_bucket(&positions, weights, cli.warmup, cli.iterations);
    }

    println!("Loading NNUE file: {}", cli.nnue_file.display());
    let network = Arc::new(NNUENetwork::load(&cli.nnue_file)?);
    let arch_name = network.architecture_name();
    println!("Architecture: {arch_name}");
    println!();

    match mode {
        BenchMode::Full => {
            let mut evaluator =
                NNUEEvaluator::new_with_position(Arc::clone(&network), &positions[0]);
            let result =
                bench_evaluator(&mut evaluator, &positions, cli.warmup, cli.iterations, arch_name);

            result.print();

            println!("--- JSON ---");
            println!(
                r#"{{"mode":"full","arch":"{}","refresh_ns":{:.1},"eval_ns":{:.1},"total_ns":{:.1},"evals_per_sec":{:.0}}}"#,
                result.arch_name,
                result.refresh_ns_per_op,
                result.eval_ns_per_op,
                result.total_ns_per_op,
                result.evals_per_sec
            );
        }
        BenchMode::LayerStackPropagate
        | BenchMode::LayerStackEval
        | BenchMode::LayerStackRefreshCache
        | BenchMode::LayerStackUpdateCache => {
            let bucket_mode = configure_layer_stack_bucket(&cli, progress_weights.as_deref())?;

            let NNUENetwork::LayerStacks(ref ls_net) = *network else {
                bail!("LayerStack 専用モードは LayerStacks NNUE のみ対応");
            };

            with_ls_net!(ls_net, |net| {
                run_layer_stack_bench(
                    net,
                    mode,
                    &positions,
                    bucket_mode,
                    cli.warmup,
                    cli.iterations,
                    arch_name,
                )?;
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_entry_is_reachable_in_tests() {
        let _ = run as fn() -> Result<()>;
    }
}
