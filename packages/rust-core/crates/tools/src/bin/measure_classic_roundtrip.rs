use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use serde::Deserialize;
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Measure Classic NNUE round-trip deltas over multiple seeds"
)]
struct Cli {
    /// Teacher network RNG seed
    #[arg(long, default_value_t = 2025)]
    teacher_seed: u64,

    /// Classic training RNG seeds (comma separated)
    #[arg(long = "classic-seed", value_delimiter = ',', default_values_t = [42u64, 43, 44, 45, 46])]
    classic_seeds: Vec<u64>,

    /// Output directory (defaults to target/classic_roundtrip_measure)
    #[arg(long)]
    out_dir: Option<PathBuf>,

    /// Round-trip threshold (max abs)
    #[arg(long, default_value_t = 400.0)]
    max_abs: f32,

    /// Round-trip threshold (mean abs)
    #[arg(long, default_value_t = 150.0)]
    mean_abs: f32,

    /// Round-trip threshold (p95 abs)
    #[arg(long, default_value_t = 250.0)]
    p95_abs: f32,

    /// Cargo profile to use when invoking sub-commands
    #[arg(long, value_enum, default_value_t = Profile::Release)]
    profile: Profile,

    /// Force re-training of the Single teacher network
    #[arg(long)]
    force_rebuild_teacher: bool,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Profile {
    Debug,
    Release,
}

#[derive(Deserialize)]
struct VerifyReport {
    actual: LayerStats,
    #[serde(default)]
    count_exceeds: Option<usize>,
}

#[derive(Deserialize)]
struct LayerStats {
    max_abs: f32,
    mean_abs: f32,
    #[serde(default)]
    p95_abs: Option<f32>,
}

#[derive(Serialize)]
struct RunMetrics {
    seed: u64,
    max_abs: f32,
    mean_abs: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    p95_abs: Option<f32>,
    count_exceeds: usize,
}

#[derive(Serialize)]
struct Summary {
    seeds: Vec<u64>,
    max_abs_max: f32,
    max_abs_mean: f32,
    mean_abs_max: f32,
    mean_abs_mean: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    p95_abs_max: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    p95_abs_mean: Option<f32>,
    count_exceeds_total: usize,
}

#[derive(Serialize)]
struct BaselineReport {
    teacher_seed: u64,
    classic_seeds: Vec<u64>,
    runs: Vec<RunMetrics>,
    summary: Summary,
    thresholds: Thresholds,
}

#[derive(Serialize)]
struct Thresholds {
    max_abs: f32,
    mean_abs: f32,
    p95_abs: f32,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    let workspace_root = canonical_workspace_root()?;
    let fixtures_dir = workspace_root.join("docs/reports/fixtures/classic_roundtrip");
    let train_path = fixtures_dir.join("train.jsonl");
    let val_path = fixtures_dir.join("val.jsonl");
    let positions_path = fixtures_dir.join("positions.sfen");

    for path in [&train_path, &val_path, &positions_path] {
        if !path.exists() {
            return Err(anyhow!("Fixture not found: {}", path.display()));
        }
    }

    let out_dir = cli
        .out_dir
        .clone()
        .unwrap_or_else(|| workspace_root.join("target/classic_roundtrip_measure"));
    fs::create_dir_all(&out_dir)?;

    let profile_flag = match cli.profile {
        Profile::Release => Some("--release"),
        Profile::Debug => None,
    };

    let teacher_dir = out_dir.join("single_teacher");
    fs::create_dir_all(&teacher_dir)?;
    let teacher_path = teacher_dir.join("nn.fp32.bin");
    if !teacher_path.exists() || cli.force_rebuild_teacher {
        log::info!("training single teacher -> {}", teacher_dir.display());
        let args = build_train_single_args(&train_path, &val_path, &teacher_dir, cli.teacher_seed);
        run_cargo(&args, profile_flag)?;
        if !teacher_path.exists() {
            return Err(anyhow!("Teacher export not found at {}", teacher_path.display()));
        }
    } else {
        log::info!("reusing existing teacher network at {}", teacher_path.display());
    }

    let mut run_metrics = Vec::new();

    for &seed in &cli.classic_seeds {
        log::info!("running classic seed {}", seed);
        let export_dir = out_dir.join(format!("export_seed{}", seed));
        fs::create_dir_all(&export_dir)?;

        let classic_args =
            build_train_classic_args(&train_path, &val_path, &export_dir, &teacher_path, seed);
        run_cargo(&classic_args, profile_flag)?;

        let fp32 = export_dir.join("nn.fp32.bin");
        let int_path = export_dir.join("nn.classic.nnue");
        let scales = export_dir.join("nn.classic.scales.json");
        for path in [&fp32, &int_path, &scales] {
            if !path.exists() {
                return Err(anyhow!("Missing export artifact: {}", path.display()));
            }
        }

        let report_path = out_dir.join(format!("roundtrip_seed{}.json", seed));
        let worst_path = out_dir.join(format!("worst_seed{}.jsonl", seed));
        let verify_args = build_verify_args(VerifyArgs {
            fp32: &fp32,
            int_path: &int_path,
            scales: &scales,
            positions: &positions_path,
            out_path: &report_path,
            worst_path: &worst_path,
            max_abs: cli.max_abs,
            mean_abs: cli.mean_abs,
            p95_abs: cli.p95_abs,
        });
        run_cargo(&verify_args, profile_flag)?;

        let metrics = load_metrics(seed, &report_path)?;
        log::info!(
            "seed {} -> max_abs {:.3}cp, mean_abs {:.3}cp, p95_abs {:?}",
            seed,
            metrics.max_abs,
            metrics.mean_abs,
            metrics.p95_abs
        );
        run_metrics.push(metrics);
    }

    let summary = summarize(&run_metrics);
    let baseline = BaselineReport {
        teacher_seed: cli.teacher_seed,
        classic_seeds: cli.classic_seeds.clone(),
        runs: run_metrics,
        summary,
        thresholds: Thresholds {
            max_abs: cli.max_abs,
            mean_abs: cli.mean_abs,
            p95_abs: cli.p95_abs,
        },
    };

    let baseline_path = out_dir.join("baseline_roundtrip.json");
    serde_json::to_writer_pretty(fs::File::create(&baseline_path)?, &baseline)?;
    log::info!("baseline report written to {}", baseline_path.display());

    Ok(())
}

fn canonical_workspace_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow!("Failed to resolve workspace root from CARGO_MANIFEST_DIR"))?
        .to_path_buf();
    Ok(root)
}

fn build_train_single_args(train: &Path, val: &Path, out_dir: &Path, seed: u64) -> Vec<String> {
    vec![
        "-p".into(),
        "tools".into(),
        "--bin".into(),
        "train_nnue".into(),
        "--".into(),
        "--input".into(),
        train.display().to_string(),
        "--validation".into(),
        val.display().to_string(),
        "--arch".into(),
        "single".into(),
        "--label".into(),
        "cp".into(),
        "--epochs".into(),
        "1".into(),
        "--batch-size".into(),
        "32".into(),
        "--opt".into(),
        "sgd".into(),
        "--rng-seed".into(),
        seed.to_string(),
        "--export-format".into(),
        "fp32".into(),
        "--metrics".into(),
        "--out".into(),
        out_dir.display().to_string(),
    ]
}

fn build_train_classic_args(
    train: &Path,
    val: &Path,
    out_dir: &Path,
    teacher: &Path,
    seed: u64,
) -> Vec<String> {
    vec![
        "-p".into(),
        "tools".into(),
        "--bin".into(),
        "train_nnue".into(),
        "--".into(),
        "--input".into(),
        train.display().to_string(),
        "--validation".into(),
        val.display().to_string(),
        "--arch".into(),
        "classic".into(),
        "--label".into(),
        "cp".into(),
        "--epochs".into(),
        "1".into(),
        "--batch-size".into(),
        "32".into(),
        "--opt".into(),
        "sgd".into(),
        "--rng-seed".into(),
        seed.to_string(),
        "--export-format".into(),
        "classic-v1".into(),
        "--emit-fp32-also".into(),
        "--distill-from-single".into(),
        teacher.display().to_string(),
        "--teacher-domain".into(),
        "cp".into(),
        "--metrics".into(),
        "--out".into(),
        out_dir.display().to_string(),
    ]
}

#[derive(Clone, Copy)]
struct VerifyArgs<'a> {
    fp32: &'a Path,
    int_path: &'a Path,
    scales: &'a Path,
    positions: &'a Path,
    out_path: &'a Path,
    worst_path: &'a Path,
    max_abs: f32,
    mean_abs: f32,
    p95_abs: f32,
}

fn build_verify_args(params: VerifyArgs) -> Vec<String> {
    let VerifyArgs {
        fp32,
        int_path,
        scales,
        positions,
        out_path,
        worst_path,
        max_abs,
        mean_abs,
        p95_abs,
    } = params;
    vec![
        "-p".into(),
        "tools".into(),
        "--bin".into(),
        "verify_classic_roundtrip".into(),
        "--".into(),
        "--fp32".into(),
        fp32.display().to_string(),
        "--int".into(),
        int_path.display().to_string(),
        "--scales".into(),
        scales.display().to_string(),
        "--positions".into(),
        positions.display().to_string(),
        "--metric".into(),
        "cp".into(),
        "--max-abs".into(),
        max_abs.to_string(),
        "--mean-abs".into(),
        mean_abs.to_string(),
        "--p95-abs".into(),
        p95_abs.to_string(),
        "--worst-count".into(),
        "20".into(),
        "--out".into(),
        out_path.display().to_string(),
        "--worst-jsonl".into(),
        worst_path.display().to_string(),
    ]
}

fn run_cargo(args: &[String], profile_flag: Option<&'static str>) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.arg("run");
    if let Some(flag) = profile_flag {
        cmd.arg(flag);
    }
    for arg in args {
        cmd.arg(arg);
    }
    println!("[measure] {}", format_command(&cmd));
    let status = cmd
        .status()
        .with_context(|| format!("failed to run {}", format_command(&cmd)))?;
    if !status.success() {
        return Err(anyhow!("command exited with status {}", status));
    }
    Ok(())
}

fn format_command(cmd: &Command) -> String {
    let mut s = String::new();
    s.push_str(cmd.get_program().to_string_lossy().as_ref());
    for arg in cmd.get_args() {
        s.push(' ');
        s.push_str(arg.to_string_lossy().as_ref());
    }
    s
}

fn load_metrics(seed: u64, path: &Path) -> Result<RunMetrics> {
    let data: VerifyReport = serde_json::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(RunMetrics {
        seed,
        max_abs: data.actual.max_abs,
        mean_abs: data.actual.mean_abs,
        p95_abs: data.actual.p95_abs,
        count_exceeds: data.count_exceeds.unwrap_or(0),
    })
}

fn summarize(runs: &[RunMetrics]) -> Summary {
    let seeds = runs.iter().map(|r| r.seed).collect::<Vec<_>>();
    let max_abs_values = runs.iter().map(|r| r.max_abs).collect::<Vec<_>>();
    let mean_abs_values = runs.iter().map(|r| r.mean_abs).collect::<Vec<_>>();
    let p95_values = runs.iter().filter_map(|r| r.p95_abs).collect::<Vec<_>>();

    Summary {
        seeds,
        max_abs_max: max_abs_values.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        max_abs_mean: mean(&max_abs_values),
        mean_abs_max: mean_abs_values.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        mean_abs_mean: mean(&mean_abs_values),
        p95_abs_max: if p95_values.is_empty() {
            None
        } else {
            Some(p95_values.iter().copied().fold(f32::NEG_INFINITY, f32::max))
        },
        p95_abs_mean: if p95_values.is_empty() {
            None
        } else {
            Some(mean(&p95_values))
        },
        count_exceeds_total: runs.iter().map(|r| r.count_exceeds).sum(),
    }
}

fn mean(values: &[f32]) -> f32 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f32>() / (values.len() as f32)
    }
}
