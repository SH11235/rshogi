//! nnue_saturation - LayerStacks NNUE の活性飽和率を実局面で計測する診断ツール。
//!
//! ClippedReLU / SqrClippedReLU 系の活性は u8 [0,127] に clamp されるため、
//! 127 到達率が高いほど量子化天井で情報が落ちている（評価値インフレの副作用の計器）。
//! 推論と同じ piece + Threat 因子 / L1→L2 / L2→output の 3 段を bucket 別に集計する。
//! Threat 無効時は piece のみ。PSQT は別経路のため含めない。
//! FT 段は SqrClippedReLU の pairing 前の因子 clamp(acc, 0, 127) が 127 に到達した割合
//! （出力は `(a*b) >> 7` で最大 126 のため、出力側では飽和を観測できない）。
//!
//! 重み側の i8/i16 飽和は rshogi-nnue (tatara) の
//! `crates/nnue-format/examples/clamp_stats.rs` が担当する（export 時量子化の話のため）。

use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use serde::Serialize;

use rshogi_core::nnue::{
    AccumulatorLayerStacks, LayerStackBucketMode, LsFeatureSpec, LsSaturationCounts, NNUENetwork,
    NetworkLayerStacks, compute_layer_stack_progresskpabs_bucket_index,
    configure_layer_stack_routing, get_layer_stack_progress_kpabs_weights,
    layer_stack_progress_coeff_required, load_progress_coeff_kpabs, ls_dispatch_ft_size,
    set_layer_stack_progress_kpabs_weights,
};
use rshogi_core::position::Position;

#[derive(Parser)]
#[command(
    name = "nnue_saturation",
    about = "LayerStacks NNUE の活性飽和率を実局面で計測"
)]
struct Cli {
    /// NNUE ファイルパス。
    #[arg(long)]
    nnue: PathBuf,

    /// SFEN ファイル（1 行 1 局面）。
    #[arg(long)]
    sfens: PathBuf,

    /// progress.bin (progresskpabs 進行度重み) のパス。
    /// `--progress-buckets 1` (no-op routing) 以外では必須。
    #[arg(long)]
    progress_coeff: Option<PathBuf>,

    /// progresskpabs が推論に使う bucket 数。
    #[arg(long)]
    progress_buckets: usize,

    /// 読む局面数の上限（省略時は全件）。
    #[arg(long)]
    count: Option<usize>,

    /// 集計 JSON の出力先（省略時は標準出力のみ）。
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct SaturationReport {
    nnue: String,
    sfens: String,
    progress_coeff: Option<String>,
    progress_buckets: usize,
    positions: u64,
    total: StageRates,
    per_bucket: Vec<BucketReport>,
}

#[derive(Debug, Serialize)]
struct BucketReport {
    bucket: usize,
    positions: u64,
    #[serde(flatten)]
    rates: StageRates,
}

/// 活性 3 段の飽和率（127 到達数 / 総数）。ft は accumulator 因子側で数える。
#[derive(Debug, Serialize)]
struct StageRates {
    ft_sat: u64,
    ft_total: u64,
    ft_rate: f64,
    l1_act_sat: u64,
    l1_act_total: u64,
    l1_act_rate: f64,
    l2_act_sat: u64,
    l2_act_total: u64,
    l2_act_rate: f64,
}

/// bucket ごとの集計（core の活性カウント + tool 側で数える FT 因子カウント）。
#[derive(Debug, Default, Clone, Copy)]
struct BucketCounts {
    ft_sat: u64,
    ft_total: u64,
    act: LsSaturationCounts,
}

impl StageRates {
    fn from_counts(c: &BucketCounts) -> Self {
        let rate = |sat: u64, total: u64| {
            if total == 0 {
                0.0
            } else {
                sat as f64 / total as f64
            }
        };
        Self {
            ft_sat: c.ft_sat,
            ft_total: c.ft_total,
            ft_rate: rate(c.ft_sat, c.ft_total),
            l1_act_sat: c.act.l1_act_sat,
            l1_act_total: c.act.l1_act_total,
            l1_act_rate: rate(c.act.l1_act_sat, c.act.l1_act_total),
            l2_act_sat: c.act.l2_act_sat,
            l2_act_total: c.act.l2_act_total,
            l2_act_rate: rate(c.act.l2_act_sat, c.act.l2_act_total),
        }
    }
}

fn merge(acc: &mut BucketCounts, c: &BucketCounts) {
    acc.ft_sat += c.ft_sat;
    acc.ft_total += c.ft_total;
    acc.act.l1_act_sat += c.act.l1_act_sat;
    acc.act.l1_act_total += c.act.l1_act_total;
    acc.act.l2_act_sat += c.act.l2_act_sat;
    acc.act.l2_act_total += c.act.l2_act_total;
}

fn run_for_network<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
    FT: LsFeatureSpec,
>(
    cli: &Cli,
    network: &NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT, FT>,
) -> Result<()> {
    let file = std::fs::File::open(&cli.sfens)
        .with_context(|| format!("sfens を開けません: {:?}", cli.sfens))?;
    let reader = BufReader::new(file);

    let mut pos = Position::new();
    let mut acc = AccumulatorLayerStacks::<L1>::new();
    let mut bucket_counts = vec![BucketCounts::default(); network.num_buckets];
    let mut bucket_positions = vec![0u64; network.num_buckets];
    let limit = cli.count.unwrap_or(usize::MAX);

    for line in reader.lines().take(limit) {
        let sfen = line?;
        let sfen = sfen.trim();
        if sfen.is_empty() {
            continue;
        }
        pos.set_sfen(sfen).with_context(|| format!("SFEN を読めません: {sfen}"))?;
        network.refresh_accumulator(&pos, &mut acc);

        let side_to_move = pos.side_to_move();
        let bucket_index = compute_layer_stack_progresskpabs_bucket_index(
            &pos,
            side_to_move,
            get_layer_stack_progress_kpabs_weights(),
            cli.progress_buckets,
        );
        let bc = &mut bucket_counts[bucket_index];
        bc.ft_sat +=
            network.accumulate_saturation_counts(&acc, side_to_move, bucket_index, &mut bc.act);
        bc.ft_total += 2 * L1 as u64;
        bucket_positions[bucket_index] += 1;
    }

    let mut total = BucketCounts::default();
    for c in &bucket_counts {
        merge(&mut total, c);
    }
    let report = SaturationReport {
        nnue: cli.nnue.display().to_string(),
        sfens: cli.sfens.display().to_string(),
        progress_coeff: cli.progress_coeff.as_ref().map(|p| p.display().to_string()),
        progress_buckets: cli.progress_buckets,
        positions: bucket_positions.iter().sum(),
        total: StageRates::from_counts(&total),
        per_bucket: bucket_counts
            .iter()
            .enumerate()
            .filter(|(_, c)| c.ft_total > 0)
            .map(|(bucket, c)| BucketReport {
                bucket,
                positions: bucket_positions[bucket],
                rates: StageRates::from_counts(c),
            })
            .collect(),
    };

    let stdout = std::io::stdout();
    let mut locked = stdout.lock();
    serde_json::to_writer_pretty(&mut locked, &report)?;
    use std::io::Write;
    writeln!(locked)?;

    if let Some(path) = &cli.out {
        let mut writer = std::io::BufWriter::new(
            std::fs::File::create(path)
                .with_context(|| format!("出力できません: {}", path.display()))?,
        );
        serde_json::to_writer_pretty(&mut writer, &report)?;
        writeln!(writer)?;
    }
    Ok(())
}

/// CLI entrypoint。
pub fn run() -> Result<()> {
    let cli = Cli::parse();

    if let Some(coeff_path) = &cli.progress_coeff {
        let weights = load_progress_coeff_kpabs(coeff_path)
            .map_err(|e| anyhow::anyhow!("--progress-coeff を読めません: {e}"))?;
        set_layer_stack_progress_kpabs_weights(weights)
            .map_err(|e| anyhow::anyhow!("progress 設定に失敗しました: {e}"))?;
    } else if layer_stack_progress_coeff_required(Some(cli.progress_buckets)) {
        anyhow::bail!("--progress-buckets が 2 以上のときは --progress-coeff が必須です");
    }
    let network = NNUENetwork::load(&cli.nnue)
        .with_context(|| format!("NNUE を読み込めません: {:?}", cli.nnue))?;
    let ls_net = match &network {
        NNUENetwork::LayerStacks(net) => net,
        _ => anyhow::bail!("nnue_saturation は LayerStacks NNUE のみ対応"),
    };
    configure_layer_stack_routing(
        LayerStackBucketMode::ProgressKPAbs,
        ls_net.num_buckets(),
        Some(cli.progress_buckets),
    )
    .map_err(anyhow::Error::msg)?;

    ls_dispatch_ft_size!(
        ls_net,
        |concrete_net| run_for_network(&cli, concrete_net),
        _ => anyhow::bail!("有効な LayerStacks (FT × L1) バリアントがありません"),
    )
}
