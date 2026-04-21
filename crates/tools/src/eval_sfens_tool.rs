//! eval_sfens - SFEN テキストファイルから指定局面を LayerStacks NNUE で評価
//!
//! # 使用方法
//!
//! ```bash
//! cargo run --release -p tools --bin eval_sfens -- \
//!   --nnue path/to/quantised.bin \
//!   --sfens path/to/sfens.txt \
//!   --count 10
//! ```

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use std::io::{BufRead, BufReader};
use std::mem::size_of;
use std::path::PathBuf;

use rshogi_core::nnue::{
    AccumulatorLayerStacks, AffineTransform, FeatureSet, HalfKA_hm_FeatureSet,
    LayerStackBucketMode, LayerStacksNetwork, NNUE_PYTORCH_L3, NNUENetwork, NetworkLayerStacks,
    SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, compute_layer_stack_progress8kpabs_bucket_index,
    get_layer_stack_progress_kpabs_weights, set_layer_stack_bucket_mode,
    set_layer_stack_progress_kpabs_weights, sqr_clipped_relu_transform,
};
use rshogi_core::position::Position;
use rshogi_core::types::Color;

#[derive(Parser)]
#[command(name = "eval_sfens", about = "LayerStacks NNUE で SFEN 局面を評価")]
struct Cli {
    /// NNUE ファイルパス
    #[arg(long)]
    nnue: PathBuf,

    /// SFEN ファイルパス（1行1局面）
    #[arg(long)]
    sfens: PathBuf,

    /// 評価する局面数
    #[arg(long, default_value_t = 10)]
    count: usize,

    /// Output bucket mode (progress8kpabs)
    #[arg(long, value_enum, default_value = "progress8kpabs")]
    bucket_mode: BucketMode,

    /// progress.bin (YaneuraOu 互換 KP-absolute 進行度重み) のパス
    #[arg(long)]
    progress_coeff: Option<PathBuf>,

    /// Dump detailed intermediates for the first evaluated position (to stderr)
    #[arg(long, default_value_t = false)]
    dump_debug_first: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BucketMode {
    Progress8kpabs,
}

fn load_progress_coeff_kpabs(path: &PathBuf) -> Result<Box<[f32]>, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("failed to read --progress-coeff '{}': {e}", path.display()))?;
    let expected = SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * size_of::<f64>();
    if bytes.len() != expected {
        return Err(format!(
            "progress.bin size mismatch: got {} bytes, expected {}",
            bytes.len(),
            expected
        ));
    }
    let weights: Vec<f32> = bytes
        .chunks_exact(size_of::<f64>())
        .map(|chunk| f64::from_le_bytes(chunk.try_into().expect("chunk size is checked")) as f32)
        .collect();
    Ok(weights.into_boxed_slice())
}

fn padded_input(input_dim: usize) -> usize {
    input_dim.div_ceil(32) * 32
}

fn scrambled_weight_index(linear: usize, padded_input_dim: usize, output_dim: usize) -> usize {
    const CHUNK_SIZE: usize = 4;
    (linear / CHUNK_SIZE) % (padded_input_dim / CHUNK_SIZE) * output_dim * CHUNK_SIZE
        + linear / padded_input_dim * CHUNK_SIZE
        + linear % CHUNK_SIZE
}

fn affine_scalar<const INPUT_DIM: usize, const OUTPUT_DIM: usize>(
    layer: &AffineTransform<INPUT_DIM, OUTPUT_DIM>,
    input: &[u8],
    output: &mut [i32; OUTPUT_DIM],
) {
    let padded = padded_input(INPUT_DIM);
    debug_assert!(input.len() >= padded);
    let use_scrambled = if cfg!(all(target_arch = "x86_64", target_feature = "avx2")) {
        OUTPUT_DIM.is_multiple_of(8) && OUTPUT_DIM > 0
    } else if cfg!(all(
        target_arch = "x86_64",
        target_feature = "ssse3",
        not(target_feature = "avx2")
    )) {
        OUTPUT_DIM.is_multiple_of(4) && OUTPUT_DIM > 0
    } else {
        false
    };
    for (j, out) in output.iter_mut().enumerate() {
        let mut acc = layer.biases[j];
        let row_offset = j * padded;
        for (i, &in_val) in input.iter().enumerate().take(INPUT_DIM) {
            let linear = row_offset + i;
            let weight_idx = if use_scrambled {
                scrambled_weight_index(linear, padded, OUTPUT_DIM)
            } else {
                linear
            };
            acc += i32::from(layer.weights[weight_idx]) * i32::from(in_val);
        }
        *out = acc;
    }
}

fn compare_i32_slices(label: &str, lhs: &[i32], rhs: &[i32]) {
    let mut mismatch = 0usize;
    let mut max_abs = 0i32;
    let mut first_mismatch: Option<usize> = None;
    for (idx, (&a, &b)) in lhs.iter().zip(rhs.iter()).enumerate() {
        let d = (a - b).abs();
        if d != 0 {
            mismatch += 1;
            if first_mismatch.is_none() {
                first_mismatch = Some(idx);
            }
            if d > max_abs {
                max_abs = d;
            }
        }
    }
    if let Some(idx) = first_mismatch {
        eprintln!(
            "[debug] {label}: mismatch={} max_abs_diff={} first_idx={} lhs={} rhs={}",
            mismatch, max_abs, idx, lhs[idx], rhs[idx]
        );
    } else {
        eprintln!("[debug] {label}: exact match");
    }
}

fn compare_i16_vs_i32(label: &str, lhs: &[i16], rhs: &[i32]) {
    let mut mismatch = 0usize;
    let mut max_abs = 0i32;
    let mut first_mismatch: Option<usize> = None;
    for (idx, (&a, &b)) in lhs.iter().zip(rhs.iter()).enumerate() {
        let d = (i32::from(a) - b).abs();
        if d != 0 {
            mismatch += 1;
            if first_mismatch.is_none() {
                first_mismatch = Some(idx);
            }
            if d > max_abs {
                max_abs = d;
            }
        }
    }
    if let Some(idx) = first_mismatch {
        eprintln!(
            "[debug] {label}: mismatch={} max_abs_diff={} first_idx={} lhs={} rhs={}",
            mismatch, max_abs, idx, lhs[idx], rhs[idx]
        );
    } else {
        eprintln!("[debug] {label}: exact match");
    }
}

fn recompute_ft_scalar<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
>(
    network: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    pos: &Position,
    perspective: Color,
) -> [i32; L1] {
    let mut acc = [0i32; L1];
    for (dst, &b) in acc.iter_mut().zip(network.feature_transformer.biases.0.iter()) {
        *dst = i32::from(b);
    }
    let active = HalfKA_hm_FeatureSet::collect_active_indices(pos, perspective);
    for index in active.iter() {
        let offset = index * L1;
        for (i, dst) in acc.iter_mut().enumerate() {
            *dst += i32::from(network.feature_transformer.weights[offset + i]);
        }
    }
    acc
}

fn dump_debug_first<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
>(
    network: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    pos: &Position,
    acc: &AccumulatorLayerStacks<L1>,
    sfen: &str,
    bucket_index: usize,
    raw: i32,
    score: i32,
) {
    eprintln!("[debug] ==================================================");
    eprintln!("[debug] sfen={sfen}");
    eprintln!("[debug] side_to_move={:?}", pos.side_to_move());
    eprintln!("[debug] bucket={bucket_index} raw={raw} score={score}");

    let black_active = HalfKA_hm_FeatureSet::collect_active_indices(pos, Color::Black);
    let white_active = HalfKA_hm_FeatureSet::collect_active_indices(pos, Color::White);
    eprintln!(
        "[debug] active_features black={} white={}",
        black_active.len(),
        white_active.len()
    );
    eprintln!(
        "[debug] active_features black first16={:?}",
        black_active.iter().take(16).collect::<Vec<_>>()
    );
    eprintln!(
        "[debug] active_features white first16={:?}",
        white_active.iter().take(16).collect::<Vec<_>>()
    );

    let black_scalar = recompute_ft_scalar(network, pos, Color::Black);
    let white_scalar = recompute_ft_scalar(network, pos, Color::White);
    compare_i16_vs_i32(
        "FT acc black(simd vs scalar)",
        acc.get(Color::Black as usize),
        &black_scalar,
    );
    compare_i16_vs_i32(
        "FT acc white(simd vs scalar)",
        acc.get(Color::White as usize),
        &white_scalar,
    );

    let side_to_move = pos.side_to_move();
    let (us_acc, them_acc) = if side_to_move == Color::Black {
        (acc.get(Color::Black as usize), acc.get(Color::White as usize))
    } else {
        (acc.get(Color::White as usize), acc.get(Color::Black as usize))
    };
    eprintln!("[debug] us_acc first16={:?}", &us_acc[..16]);
    eprintln!("[debug] them_acc first16={:?}", &them_acc[..16]);

    let mut transformed = [0u8; L1];
    sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed);
    eprintln!("[debug] transformed first32={:?}", &transformed[..32]);

    let bucket = &network.layer_stacks.buckets[bucket_index];

    let main_dim = LS_L1_OUT - 1;
    let mut l1_simd = [0i32; LS_L1_OUT];
    let mut l1_scalar = [0i32; LS_L1_OUT];
    bucket.l1.propagate(&transformed, &mut l1_simd);
    affine_scalar(&bucket.l1, &transformed, &mut l1_scalar);
    compare_i32_slices("LayerStack L1(simd vs scalar)", &l1_simd, &l1_scalar);
    eprintln!("[debug] l1_out={l1_simd:?}");

    let l1_skip = l1_simd[main_dim];
    let mut l2_input = [0u8; LS_L2_PADDED_INPUT];
    for i in 0..main_dim {
        let v = i64::from(l1_simd[i]);
        let sqr = ((v * v) >> 19).clamp(0, 127) as u8;
        let crelu = (l1_simd[i] >> 6).clamp(0, 127) as u8;
        l2_input[i] = sqr;
        l2_input[main_dim + i] = crelu;
    }
    eprintln!("[debug] l2_input[0..{LS_L2_IN}]={:?}", &l2_input[..LS_L2_IN]);

    let mut l2_simd = [0i32; NNUE_PYTORCH_L3];
    let mut l2_scalar = [0i32; NNUE_PYTORCH_L3];
    bucket.l2.propagate(&l2_input, &mut l2_simd);
    affine_scalar(&bucket.l2, &l2_input, &mut l2_scalar);
    compare_i32_slices("LayerStack L2(simd vs scalar)", &l2_simd, &l2_scalar);
    eprintln!("[debug] l2_out first16={:?}", &l2_simd[..16]);

    let mut l2_relu = [0u8; NNUE_PYTORCH_L3];
    for (dst, &v) in l2_relu.iter_mut().zip(l2_simd.iter()) {
        *dst = (v >> 6).clamp(0, 127) as u8;
    }

    let mut out_simd = [0i32; 1];
    let mut out_scalar = [0i32; 1];
    bucket.output.propagate(&l2_relu, &mut out_simd);
    affine_scalar(&bucket.output, &l2_relu, &mut out_scalar);
    compare_i32_slices("LayerStack Output(simd vs scalar)", &out_simd, &out_scalar);

    let raw_reconstructed = out_simd[0] + l1_skip;
    eprintln!(
        "[debug] raw_reconstructed={} (output={} + skip={})",
        raw_reconstructed, out_simd[0], l1_skip
    );
    eprintln!("[debug] raw_from_network={raw}");
    eprintln!("[debug] ==================================================");
}

fn run_eval_for_network<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
>(
    cli: &Cli,
    network: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
) -> Result<()> {
    eprintln!("fv_scale: {}", network.fv_scale);
    eprintln!(
        "bucket_mode: {}",
        match cli.bucket_mode {
            BucketMode::Progress8kpabs => "progress8kpabs",
        }
    );

    let file = std::fs::File::open(&cli.sfens)
        .with_context(|| format!("Failed to open: {:?}", cli.sfens))?;
    let reader = BufReader::new(file);

    let mut pos = Position::new();
    let mut acc = AccumulatorLayerStacks::<L1>::new();
    let mut dumped_debug = false;

    println!("sfen\tbucket\traw\tscore");
    for (i, line) in reader.lines().enumerate() {
        if i >= cli.count {
            break;
        }
        let sfen = line?;
        let sfen = sfen.trim().to_string();
        if sfen.is_empty() {
            continue;
        }

        pos.set_sfen(&sfen).with_context(|| format!("Invalid SFEN: {sfen}"))?;
        network.refresh_accumulator(&pos, &mut acc);

        let value = network.evaluate(&pos, &acc);

        let side_to_move = pos.side_to_move();
        let (us_acc, them_acc) = if side_to_move == Color::Black {
            (acc.get(Color::Black as usize), acc.get(Color::White as usize))
        } else {
            (acc.get(Color::White as usize), acc.get(Color::Black as usize))
        };

        let mut transformed = [0u8; L1];
        sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed);

        let bucket_index = match cli.bucket_mode {
            BucketMode::Progress8kpabs => {
                let weights = get_layer_stack_progress_kpabs_weights();
                compute_layer_stack_progress8kpabs_bucket_index(&pos, side_to_move, weights)
            }
        };
        let raw = network.layer_stacks.evaluate_raw(bucket_index, &transformed);

        if cli.dump_debug_first && !dumped_debug {
            dump_debug_first(network, &pos, &acc, &sfen, bucket_index, raw, value.raw());
            dumped_debug = true;
        }

        println!("{sfen}\t{bucket_index}\t{raw}\t{}", value.raw());
    }

    Ok(())
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.bucket_mode {
        BucketMode::Progress8kpabs => {
            let coeff_path = cli.progress_coeff.as_ref().ok_or_else(|| {
                anyhow::anyhow!("--bucket-mode progress8kpabs requires --progress-coeff")
            })?;
            let weights = load_progress_coeff_kpabs(coeff_path)
                .map_err(|e| anyhow::anyhow!("failed to load --progress-coeff: {e}"))?;
            set_layer_stack_progress_kpabs_weights(weights)
                .map_err(|e| anyhow::anyhow!("failed to set kpabs weights: {e}"))?;
            set_layer_stack_bucket_mode(LayerStackBucketMode::Progress8KPAbs);
        }
    }

    eprintln!("Loading NNUE: {:?}", cli.nnue);
    let network = NNUENetwork::load(&cli.nnue)
        .with_context(|| format!("Failed to load NNUE: {:?}", cli.nnue))?;
    let ls_net = match &network {
        NNUENetwork::LayerStacks(net) => net,
        _ => anyhow::bail!("eval_sfens は LayerStacks NNUE のみ対応"),
    };

    macro_rules! with_network {
        ($net:expr, |$inner:ident| $body:expr) => {
            match $net {
                #[cfg(feature = "layerstacks-1536x16x32")]
                LayerStacksNetwork::L1536x16x32($inner) => $body,
                #[cfg(feature = "layerstacks-1536x32x32")]
                LayerStacksNetwork::L1536x32x32($inner) => $body,
                #[cfg(feature = "layerstacks-768x16x32")]
                LayerStacksNetwork::L768($inner) => $body,
                #[cfg(feature = "layerstacks-512x16x32")]
                LayerStacksNetwork::L512($inner) => $body,
                #[allow(unreachable_patterns)]
                _ => anyhow::bail!("有効な LayerStacks バリアントがありません"),
            }
        };
    }

    with_network!(ls_net, |concrete_net| run_eval_for_network(&cli, concrete_net))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_entry_is_reachable_in_tests() {
        let _ = run as fn() -> Result<()>;
    }
}
