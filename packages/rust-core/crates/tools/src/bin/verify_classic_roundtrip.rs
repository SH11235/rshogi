use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, ValueEnum};
use engine_core::evaluation::nnue::features::flip_us_them;
use engine_core::usi::parse_sfen;
use engine_core::Position;
use log::warn;
use rand::{Rng, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;
use serde::Serialize;
use tools::classic_roundtrip::{extract_feature_indices, ClassicFp32Network, ClassicIntNetwork};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Compare Classic FP32 and INT NNUE outputs over a position suite"
)]
struct Cli {
    /// Path to FP32 Classic network (nn.fp32.bin)
    #[arg(long)]
    fp32: PathBuf,

    /// Path to Classic INT network (nn.classic.nnue)
    #[arg(long)]
    int: PathBuf,

    /// Path to quantization scales JSON (defaults to sibling nn.classic.scales.json)
    #[arg(long)]
    scales: Option<PathBuf>,

    /// SFEN list (one per line)
    #[arg(long)]
    positions: Option<PathBuf>,

    /// Enable synthetic probe mode (generates synthetic feature activations)
    #[arg(long)]
    synthetic_probe: bool,

    /// Number of synthetic samples generated per activation pattern (only with --synthetic-probe)
    #[arg(long, default_value_t = 128)]
    probe_count: usize,

    /// RNG seed for synthetic probe generation (0 => deterministic default)
    #[arg(long, default_value_t = 0)]
    probe_seed: u64,

    /// Metric namespace (currently informational)
    #[arg(long, value_enum, default_value_t = MetricKind::Cp)]
    metric: MetricKind,

    /// Max absolute diff threshold (centipawn/logit)
    #[arg(long)]
    max_abs: Option<f32>,

    /// Mean absolute diff threshold
    #[arg(long)]
    mean_abs: Option<f32>,

    /// 95th percentile absolute diff threshold
    #[arg(long)]
    p95_abs: Option<f32>,

    /// Report output path (JSON)
    #[arg(long)]
    out: Option<PathBuf>,

    /// Worst case JSONL (one entry per line)
    #[arg(long)]
    worst_jsonl: Option<PathBuf>,

    /// Number of worst SFENs to record
    #[arg(long, default_value_t = 10)]
    worst_count: usize,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum MetricKind {
    Cp,
    Logit,
}

struct ProbeCase {
    label: String,
    features_us: Vec<usize>,
    features_them: Vec<usize>,
}

#[derive(Default)]
struct StatCollector {
    values: Vec<f32>,
}

impl StatCollector {
    fn push(&mut self, diff: f32) {
        self.values.push(diff.abs());
    }

    fn extend<I: IntoIterator<Item = f32>>(&mut self, iter: I) {
        for diff in iter {
            self.push(diff);
        }
    }

    fn summary(&self) -> Option<Stats> {
        if self.values.is_empty() {
            return None;
        }
        let max_abs = self.values.iter().copied().fold(0.0_f32, |m, v| if v > m { v } else { m });
        let mean_abs = self.values.iter().copied().sum::<f32>() / self.values.len() as f32;
        let mut sorted = self.values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p95 = percentile(&sorted, 0.95);
        Some(Stats {
            max_abs,
            mean_abs,
            p95_abs: p95,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
struct LayerStats {
    max_abs: f32,
    mean_abs: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    p95_abs: Option<f32>,
}

#[derive(Debug, Clone)]
struct Stats {
    max_abs: f32,
    mean_abs: f32,
    p95_abs: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
struct ThresholdReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_abs: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_abs: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    p95_abs: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
struct LayersReport {
    ft: LayerStats,
    h1: LayerStats,
    h2: LayerStats,
    out: LayerStats,
}

#[derive(Debug, Clone, Serialize)]
struct EnvReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    git: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu: Option<String>,
    features: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct WorstEntry {
    #[serde(rename = "sfen")]
    label: String,
    abs_cp: f32,
    fp32: f32,
    int: f32,
}

#[derive(Debug, Clone, Serialize)]
struct RoundTripReport {
    n: usize,
    mode: String,
    layer_domain: String,
    metric: String,
    threshold: ThresholdReport,
    actual: LayerStats,
    count_exceeds: usize,
    layers: LayersReport,
    env: EnvReport,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    worst: Vec<WorstEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    synthetic: Option<SyntheticProbeReport>,
}

#[derive(Debug, Clone, Serialize)]
struct SyntheticProbeReport {
    per_combo: usize,
    seed: u64,
    combos: Vec<usize>,
}

struct SampleDiff {
    label: String,
    diff: f32,
    fp32: f32,
    int: f32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if !cli.synthetic_probe && cli.positions.is_none() {
        bail!("--positions is required unless --synthetic-probe is set");
    }
    if cli.synthetic_probe && cli.positions.is_some() {
        bail!("--positions cannot be combined with --synthetic-probe");
    }

    let fp32_net = ClassicFp32Network::load(&cli.fp32)?;
    let int_net = ClassicIntNetwork::load(&cli.int, cli.scales.clone())?;

    let relu_clip = fp32_net.relu_clip();
    let int_scales = int_net.scales();
    let h1_scale = int_scales.s_in_2;
    let h2_scale = int_scales.s_in_3;

    const DEFAULT_SYNTHETIC_SEED: u64 = 0xC0FF_EE12_D15C_A11C;
    const SYNTHETIC_COMBOS: [usize; 4] = [0, 1, 2, 4];

    let (cases, synthetic_report) = if cli.synthetic_probe {
        let seed = if cli.probe_seed == 0 {
            DEFAULT_SYNTHETIC_SEED
        } else {
            cli.probe_seed
        };
        let cases =
            build_synthetic_cases(fp32_net.input_dim, &SYNTHETIC_COMBOS, cli.probe_count, seed)?;
        if cases.is_empty() {
            bail!("synthetic probe generated no probes (input_dim={})", fp32_net.input_dim);
        }
        (
            cases,
            Some(SyntheticProbeReport {
                per_combo: cli.probe_count,
                seed,
                combos: SYNTHETIC_COMBOS.to_vec(),
            }),
        )
    } else {
        let path = cli.positions.as_ref().expect("positions path validated");
        let positions = load_positions(path)?;
        if positions.is_empty() {
            bail!("no SFEN entries found in {}", path.display());
        }
        let mut cases = Vec::with_capacity(positions.len());
        for (sfen, pos) in positions {
            let (features_us, features_them) = extract_feature_indices(&pos)
                .with_context(|| format!("extracting features for SFEN {sfen}"))?;
            cases.push(ProbeCase {
                label: sfen,
                features_us,
                features_them,
            });
        }
        (cases, None)
    };

    let mode = if cli.synthetic_probe {
        "synthetic"
    } else {
        "positions"
    };

    let mut output_stats = StatCollector::default();
    let mut ft_stats = StatCollector::default();
    let mut h1_stats = StatCollector::default();
    let mut h2_stats = StatCollector::default();
    let mut worst: Vec<SampleDiff> = Vec::new();
    let mut exceeds_count = 0usize;

    for case in &cases {
        let fp32_layers = fp32_net.forward(&case.features_us, &case.features_them);
        let int_layers = int_net.forward(&case.features_us, &case.features_them);

        let diff = fp32_layers.output - int_layers.output;
        output_stats.push(diff);

        if let Some(max_thr) = cli.max_abs {
            if diff.abs() > max_thr {
                exceeds_count += 1;
            }
        }

        ft_stats.extend(fp32_layers.ft.iter().zip(int_layers.ft.iter()).map(|(a, b)| a - b));

        for (idx, (&fp, &int_val)) in fp32_layers.h1.iter().zip(int_layers.h1.iter()).enumerate() {
            let diff = quantized_residual(fp, int_val, h1_scale, relu_clip);
            if diff.is_nan() {
                warn!("h1 diff became NaN at {} index {}", case.label, idx);
            }
            h1_stats.push(diff);
        }

        for (idx, (&fp, &int_val)) in fp32_layers.h2.iter().zip(int_layers.h2.iter()).enumerate() {
            let diff = quantized_residual(fp, int_val, h2_scale, relu_clip);
            if diff.is_nan() {
                warn!("h2 diff became NaN at {} index {}", case.label, idx);
            }
            h2_stats.push(diff);
        }

        worst.push(SampleDiff {
            label: case.label.clone(),
            diff,
            fp32: fp32_layers.output,
            int: int_layers.output,
        });
    }

    let actual_stats = output_stats.summary().expect("output statistics should not be empty");
    let ft_stats = ft_stats.summary().unwrap_or(Stats::zero());
    let h1_stats = h1_stats.summary().unwrap_or(Stats::zero());
    let h2_stats = h2_stats.summary().unwrap_or(Stats::zero());

    let threshold = ThresholdReport {
        max_abs: cli.max_abs,
        mean_abs: cli.mean_abs,
        p95_abs: cli.p95_abs,
    };

    let actual_layer = to_layer_stats(&actual_stats);
    let layers = LayersReport {
        ft: to_layer_stats(&ft_stats),
        h1: to_layer_stats(&h1_stats),
        h2: to_layer_stats(&h2_stats),
        out: actual_layer.clone(),
    };

    let mean_exceeds = cli.mean_abs.map(|thr| actual_stats.mean_abs > thr).unwrap_or(false);
    let p95_exceeds = match (cli.p95_abs, actual_stats.p95_abs) {
        (Some(thr), Some(p95)) => p95 > thr,
        _ => false,
    };

    worst.sort_by(|a, b| b.diff.abs().partial_cmp(&a.diff.abs()).unwrap());
    worst.truncate(cli.worst_count);
    let worst_serialized: Vec<WorstEntry> = worst
        .iter()
        .map(|entry| WorstEntry {
            label: entry.label.clone(),
            abs_cp: entry.diff.abs(),
            fp32: entry.fp32,
            int: entry.int,
        })
        .collect();

    if let Some(path) = &cli.worst_jsonl {
        let mut file = File::create(path)
            .with_context(|| format!("failed to create worst JSONL at {}", path.display()))?;
        for entry in &worst_serialized {
            serde_json::to_writer(&mut file, entry)?;
            file.write_all(b"\n")?;
        }
    }

    let mut env_features = vec![
        format!("metric={:?}", cli.metric),
        format!("mode={mode}"),
        String::from("layer_diff=quantized"),
    ];
    if let Some(synth) = &synthetic_report {
        env_features.push(format!("probe_per_combo={}", synth.per_combo));
        env_features.push(format!("probe_seed=0x{:016X}", synth.seed));
    }

    let env = EnvReport {
        git: detect_git_rev(),
        cpu: detect_cpu_brand(),
        features: env_features,
    };

    let report = RoundTripReport {
        n: cases.len(),
        mode: mode.to_string(),
        layer_domain: String::from("quantized-dequantized"),
        metric: cli.metric.to_string(),
        threshold: threshold.clone(),
        actual: actual_layer,
        count_exceeds: exceeds_count,
        layers,
        env,
        worst: worst_serialized.clone(),
        synthetic: synthetic_report.clone(),
    };

    if let Some(path) = &cli.out {
        let file = File::create(path)
            .with_context(|| format!("failed to create report file at {}", path.display()))?;
        serde_json::to_writer_pretty(file, &report)?;
    }

    let label = if cli.synthetic_probe {
        "probes"
    } else {
        "positions"
    };
    println!(
        "{}: {} | max_abs: {:.4} | mean_abs: {:.4} | p95_abs: {:.4?} | exceeds: {}",
        label,
        cases.len(),
        actual_stats.max_abs,
        actual_stats.mean_abs,
        actual_stats.p95_abs,
        exceeds_count
    );

    let should_fail = (!worst.is_empty() && cli.max_abs.is_some() && exceeds_count > 0)
        || mean_exceeds
        || p95_exceeds;

    if mean_exceeds {
        eprintln!("mean_abs {} exceeded threshold {:?}", actual_stats.mean_abs, cli.mean_abs);
    }
    if p95_exceeds {
        eprintln!("p95_abs {:?} exceeded threshold {:?}", actual_stats.p95_abs, cli.p95_abs);
    }
    if cli.max_abs.is_some() && exceeds_count > 0 {
        eprintln!("{} {} exceeded max_abs threshold {:?}", exceeds_count, label, cli.max_abs);
    }

    if should_fail {
        std::process::exit(1);
    }

    Ok(())
}

fn to_layer_stats(stats: &Stats) -> LayerStats {
    LayerStats {
        max_abs: stats.max_abs,
        mean_abs: stats.mean_abs,
        p95_abs: stats.p95_abs,
    }
}

fn percentile(sorted: &[f32], q: f32) -> Option<f32> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((sorted.len() - 1) as f32 * q).round() as usize;
    Some(sorted[rank.min(sorted.len() - 1)])
}

fn load_positions(path: &PathBuf) -> Result<Vec<(String, Position)>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open positions file: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let pos =
            parse_sfen(trimmed).map_err(|e| anyhow!("failed to parse SFEN '{}': {e}", trimmed))?;
        out.push((trimmed.to_string(), pos));
    }
    Ok(out)
}

const MAX_I8_F: f32 = 127.0;

fn quantized_residual(fp: f32, int_val: f32, scale: f32, clip: f32) -> f32 {
    if !scale.is_finite() || scale <= 0.0 {
        return fp - int_val;
    }
    let fp_q = quantize_to_i8(fp, scale, clip);
    let int_q = quantize_to_i8(int_val, scale, clip);
    ((fp_q as f32) - (int_q as f32)) * scale
}

fn quantize_to_i8(value: f32, scale: f32, clip: f32) -> i8 {
    if !scale.is_finite() || scale <= 0.0 {
        return 0;
    }
    let clipped = value.clamp(0.0, clip.max(0.0));
    let quant = (clipped / scale).round().clamp(0.0, MAX_I8_F);
    quant as i8
}

fn build_synthetic_cases(
    input_dim: usize,
    combos: &[usize],
    per_combo: usize,
    seed: u64,
) -> Result<Vec<ProbeCase>> {
    if input_dim == 0 {
        bail!("input_dim is zero; cannot build synthetic probes");
    }

    let mut cases = Vec::new();
    let mut counter: usize = 0;
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);

    for &k in combos {
        if k == 0 {
            cases.push(ProbeCase {
                label: format!("probe#{:05}_0hot", counter),
                features_us: Vec::new(),
                features_them: Vec::new(),
            });
            counter += 1;
            continue;
        }

        if k > input_dim {
            continue;
        }

        if k == 1 {
            let limit = per_combo.min(input_dim);
            for idx in 0..limit {
                let features_us = vec![idx];
                let features_them = vec![flip_us_them(idx)];
                cases.push(ProbeCase {
                    label: format_probe_label(counter, &features_us),
                    features_us,
                    features_them,
                });
                counter += 1;
            }
            if per_combo > 0 && input_dim > limit {
                for _ in 0..per_combo {
                    let idx = rng.random_range(0..input_dim);
                    let features_us = vec![idx];
                    let features_them = vec![flip_us_them(idx)];
                    cases.push(ProbeCase {
                        label: format_probe_label(counter, &features_us),
                        features_us,
                        features_them,
                    });
                    counter += 1;
                }
            }
            continue;
        }

        if per_combo == 0 {
            continue;
        }

        for _ in 0..per_combo {
            let mut set = BTreeSet::new();
            while set.len() < k {
                set.insert(rng.random_range(0..input_dim));
            }
            let features_us: Vec<usize> = set.into_iter().collect();
            let mut features_them: Vec<usize> =
                features_us.iter().map(|&f| flip_us_them(f)).collect();
            features_them.sort_unstable();
            features_them.dedup();
            cases.push(ProbeCase {
                label: format_probe_label(counter, &features_us),
                features_us,
                features_them,
            });
            counter += 1;
        }
    }

    Ok(cases)
}

fn format_probe_label(index: usize, features: &[usize]) -> String {
    if features.is_empty() {
        return format!("probe#{:05}_0hot", index);
    }
    let kind = features.len();
    let summary = if features.len() <= 4 {
        features.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("-")
    } else {
        format!("{}indices", features.len())
    };
    format!("probe#{:05}_{}hot_{}", index, kind, summary)
}

fn detect_git_rev() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                None
            }
        })
}

fn detect_cpu_brand() -> Option<String> {
    #[cfg(target_arch = "x86_64")]
    {
        std::process::Command::new("lscpu").output().ok().and_then(|out| {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                for line in stdout.lines() {
                    if let Some(idx) = line.find("Model name:") {
                        return Some(line[idx + 11..].trim().to_string());
                    }
                }
                None
            } else {
                None
            }
        })
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        None
    }
}

impl Stats {
    fn zero() -> Self {
        Stats {
            max_abs: 0.0,
            mean_abs: 0.0,
            p95_abs: None,
        }
    }
}

impl std::fmt::Display for MetricKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetricKind::Cp => write!(f, "cp"),
            MetricKind::Logit => write!(f, "logit"),
        }
    }
}
