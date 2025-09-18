use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, ValueEnum};
use engine_core::usi::parse_sfen;
use engine_core::Position;
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
    positions: PathBuf,

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
    sfen: String,
    abs_cp: f32,
    fp32: f32,
    int: f32,
}

#[derive(Debug, Clone, Serialize)]
struct RoundTripReport {
    n: usize,
    metric: String,
    threshold: ThresholdReport,
    actual: LayerStats,
    count_exceeds: usize,
    layers: LayersReport,
    env: EnvReport,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    worst: Vec<WorstEntry>,
}

struct SampleDiff {
    sfen: String,
    diff: f32,
    fp32: f32,
    int: f32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let fp32_net = ClassicFp32Network::load(&cli.fp32)?;
    let int_net = ClassicIntNetwork::load(&cli.int, cli.scales.clone())?;

    let positions = load_positions(&cli.positions)?;
    if positions.is_empty() {
        bail!("no SFEN entries found in {}", cli.positions.display());
    }

    let mut output_stats = StatCollector::default();
    let mut ft_stats = StatCollector::default();
    let mut h1_stats = StatCollector::default();
    let mut h2_stats = StatCollector::default();
    let mut worst: Vec<SampleDiff> = Vec::new();
    let mut exceeds_count = 0usize;

    for (sfen, pos) in &positions {
        let (features_us, features_them) = extract_feature_indices(pos)
            .with_context(|| format!("extracting features for SFEN {sfen}"))?;
        let fp32_layers = fp32_net.forward(&features_us, &features_them);
        let int_layers = int_net.forward(&features_us, &features_them);

        let diff = fp32_layers.output - int_layers.output;
        output_stats.push(diff);

        if let Some(max_thr) = cli.max_abs {
            if diff.abs() > max_thr {
                exceeds_count += 1;
            }
        }

        ft_stats.extend(fp32_layers.ft.iter().zip(int_layers.ft.iter()).map(|(a, b)| a - b));
        h1_stats.extend(fp32_layers.h1.iter().zip(int_layers.h1.iter()).map(|(a, b)| a - b));
        h2_stats.extend(fp32_layers.h2.iter().zip(int_layers.h2.iter()).map(|(a, b)| a - b));

        worst.push(SampleDiff {
            sfen: sfen.clone(),
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
            sfen: entry.sfen.clone(),
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

    let env = EnvReport {
        git: detect_git_rev(),
        cpu: detect_cpu_brand(),
        features: vec![format!("metric={:?}", cli.metric)],
    };

    let report = RoundTripReport {
        n: positions.len(),
        metric: cli.metric.to_string(),
        threshold: threshold.clone(),
        actual: actual_layer,
        count_exceeds: exceeds_count,
        layers,
        env,
        worst: worst_serialized.clone(),
    };

    if let Some(path) = &cli.out {
        let file = File::create(path)
            .with_context(|| format!("failed to create report file at {}", path.display()))?;
        serde_json::to_writer_pretty(file, &report)?;
    }

    println!(
        "positions: {} | max_abs: {:.4} | mean_abs: {:.4} | p95_abs: {:.4?} | exceeds: {}",
        positions.len(),
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
        eprintln!("{} positions exceeded max_abs threshold {:?}", exceeds_count, cli.max_abs);
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
