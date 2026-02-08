use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Parser;
use rand::prelude::IndexedRandom;
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use tools::selfplay::game::{run_game, GameConfig, MoveEvent};
use tools::selfplay::time_control::TimeControl;
use tools::selfplay::{
    load_start_positions, EngineConfig, EngineProcess, GameOutcome, ParsedPosition,
};

const PARAM_NOT_USED_MARKER: &str = "[[NOT USED]]";
const META_FORMAT_VERSION: u32 = 1;

#[derive(Parser, Debug)]
#[command(author, version, about = "SPSA tuner for USI engines")]
struct Cli {
    /// SPSAパラメータファイル（name,type,v,min,max,step,delta）
    #[arg(long)]
    params: PathBuf,

    /// 反復回数
    #[arg(long, default_value_t = 1)]
    iterations: u32,

    /// 1イテレーションあたり対局数（偶数必須）
    #[arg(long, default_value_t = 2)]
    games_per_iteration: u32,

    /// 摂動スケール
    #[arg(long, default_value_t = 1.0)]
    scale: f64,

    /// 更新移動量スケール
    #[arg(long, default_value_t = 1.0)]
    mobility: f64,

    /// A系列の a（a_t = a / (A + t)^alpha）
    #[arg(long, default_value_t = 0.2)]
    a: f64,

    /// A系列の A（a_t = a / (A + t)^alpha）
    #[arg(long = "a-offset", default_value_t = 50.0)]
    a_offset: f64,

    /// A系列の alpha（a_t = a / (A + t)^alpha）
    #[arg(long, default_value_t = 0.602)]
    alpha: f64,

    /// c系列の c（c_t = c / t^gamma）
    #[arg(long, default_value_t = 1.0)]
    c: f64,

    /// c系列の gamma（c_t = c / t^gamma）
    #[arg(long, default_value_t = 0.101)]
    gamma: f64,

    /// 再開メタデータファイル（既定: <params>.meta.json）
    #[arg(long)]
    meta_file: Option<PathBuf>,

    /// 既存メタデータから反復番号を再開する
    #[arg(long, default_value_t = false)]
    resume: bool,

    /// 反復統計CSVの出力先（resume時は追記）
    #[arg(long)]
    stats_csv: Option<PathBuf>,

    /// 反復統計のseed横断集計CSV（平均・分散）
    #[arg(long)]
    stats_aggregate_csv: Option<PathBuf>,

    /// 乱数seed（単一）
    #[arg(long, conflicts_with = "seeds")]
    seed: Option<u64>,

    /// 乱数seed一覧（カンマ区切り）
    #[arg(long, value_delimiter = ',', num_args = 1.., conflicts_with = "seed")]
    seeds: Option<Vec<u64>>,

    /// エンジンバイナリパス（未指定時: target/release/rshogi-usi）
    #[arg(long)]
    engine_path: Option<PathBuf>,

    /// エンジン追加引数
    #[arg(long, num_args = 1..)]
    engine_args: Option<Vec<String>>,

    /// Threads option
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Hash/USI_Hash (MiB)
    #[arg(long, default_value_t = 256)]
    hash_mb: u32,

    /// 秒読み(ms)
    #[arg(long, default_value_t = 1000)]
    byoyomi: u64,

    /// 1局あたり最大手数
    #[arg(long, default_value_t = 320)]
    max_moves: u32,

    /// タイムアウト判定マージン(ms)
    #[arg(long, default_value_t = 1000)]
    timeout_margin_ms: u64,

    /// 開始局面ファイル
    #[arg(long)]
    startpos_file: Option<PathBuf>,

    /// 単一開始局面（position行またはSFEN）
    #[arg(long)]
    sfen: Option<String>,

    /// 開始局面をランダム選択
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    random_startpos: bool,
}

#[derive(Clone, Debug)]
struct SpsaParam {
    name: String,
    type_name: String,
    is_int: bool,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    delta: f64,
    comment: String,
    not_used: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct ScheduleConfig {
    a: f64,
    a_offset: f64,
    alpha: f64,
    c: f64,
    gamma: f64,
    scale: f64,
    mobility: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ResumeMetaData {
    format_version: u32,
    params_file: String,
    completed_iterations: u32,
    total_games: usize,
    last_step_sum: f64,
    last_grad_scale: f64,
    last_a_t: f64,
    last_c_t: f64,
    updated_at_utc: String,
    schedule: ScheduleConfig,
}

#[derive(Clone, Copy, Debug)]
struct IterationStats {
    iteration: u32,
    seed: u64,
    games: u32,
    plus_wins: u32,
    minus_wins: u32,
    draws: u32,
    step_sum: f64,
    grad_scale: f64,
    a_t: f64,
    c_t: f64,
    active_params: usize,
    avg_abs_shift: f64,
    updated_params: usize,
    avg_abs_update: f64,
    max_abs_update: f64,
    total_games: usize,
}

#[derive(Clone, Copy, Debug)]
struct AggregateIterationStats {
    iteration: u32,
    seed_count: usize,
    games_per_seed: u32,
    step_sum_mean: f64,
    step_sum_variance: f64,
    grad_scale_mean: f64,
    grad_scale_variance: f64,
    plus_wins_mean: f64,
    plus_wins_variance: f64,
    minus_wins_mean: f64,
    minus_wins_variance: f64,
    draws_mean: f64,
    draws_variance: f64,
    total_games: usize,
}

fn default_meta_path(params_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.meta.json", params_path.display()))
}

fn write_stats_csv_header(writer: &mut BufWriter<File>) -> Result<()> {
    writeln!(
        writer,
        "iteration,seed,games,plus_wins,minus_wins,draws,step_sum,grad_scale,a_t,c_t,active_params,\
         avg_abs_shift,updated_params,avg_abs_update,max_abs_update,total_games"
    )?;
    Ok(())
}

fn write_stats_aggregate_csv_header(writer: &mut BufWriter<File>) -> Result<()> {
    writeln!(
        writer,
        "iteration,seeds,games_per_seed,step_sum_mean,step_sum_variance,grad_scale_mean,grad_scale_variance,\
         plus_wins_mean,plus_wins_variance,minus_wins_mean,minus_wins_variance,draws_mean,draws_variance,total_games"
    )?;
    Ok(())
}

fn open_stats_csv_writer(path: &Path, resume: bool) -> Result<BufWriter<File>> {
    let write_header = if resume {
        if !path.exists() {
            true
        } else {
            std::fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .len()
                == 0
        }
    } else {
        true
    };
    let file = if resume {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {} for append", path.display()))?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?
    };
    let mut writer = BufWriter::new(file);
    if write_header {
        write_stats_csv_header(&mut writer)?;
        writer.flush()?;
    }
    Ok(writer)
}

fn open_stats_aggregate_csv_writer(path: &Path, resume: bool) -> Result<BufWriter<File>> {
    let write_header = if resume {
        if !path.exists() {
            true
        } else {
            std::fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .len()
                == 0
        }
    } else {
        true
    };
    let file = if resume {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {} for append", path.display()))?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?
    };
    let mut writer = BufWriter::new(file);
    if write_header {
        write_stats_aggregate_csv_header(&mut writer)?;
        writer.flush()?;
    }
    Ok(writer)
}

fn write_stats_csv_row(writer: &mut BufWriter<File>, stats: IterationStats) -> Result<()> {
    writeln!(
        writer,
        "{},{},{},{},{},{},{:+.6},{:+.6},{:.6},{:.6},{},{:.6},{},{:.6},{:.6},{}",
        stats.iteration,
        stats.seed,
        stats.games,
        stats.plus_wins,
        stats.minus_wins,
        stats.draws,
        stats.step_sum,
        stats.grad_scale,
        stats.a_t,
        stats.c_t,
        stats.active_params,
        stats.avg_abs_shift,
        stats.updated_params,
        stats.avg_abs_update,
        stats.max_abs_update,
        stats.total_games
    )?;
    Ok(())
}

fn write_stats_aggregate_csv_row(
    writer: &mut BufWriter<File>,
    stats: AggregateIterationStats,
) -> Result<()> {
    writeln!(
        writer,
        "{},{},{},{:+.6},{:.6},{:+.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{}",
        stats.iteration,
        stats.seed_count,
        stats.games_per_seed,
        stats.step_sum_mean,
        stats.step_sum_variance,
        stats.grad_scale_mean,
        stats.grad_scale_variance,
        stats.plus_wins_mean,
        stats.plus_wins_variance,
        stats.minus_wins_mean,
        stats.minus_wins_variance,
        stats.draws_mean,
        stats.draws_variance,
        stats.total_games
    )?;
    Ok(())
}

fn load_meta(path: &Path) -> Result<ResumeMetaData> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let meta = serde_json::from_reader(reader)
        .with_context(|| format!("failed to parse JSON {}", path.display()))?;
    Ok(meta)
}

fn save_meta(path: &Path, meta: &ResumeMetaData) -> Result<()> {
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, meta)
        .with_context(|| format!("failed to write JSON {}", path.display()))?;
    Ok(())
}

#[inline]
fn schedule_values(config: ScheduleConfig, iteration_index: u32) -> (f64, f64) {
    let t = iteration_index as f64 + 1.0;
    let a_t = config.a / (config.a_offset + t).powf(config.alpha);
    let c_t = config.c / t.powf(config.gamma);
    (a_t, c_t)
}

fn parse_param_line(line: &str, line_no: usize) -> Result<Option<SpsaParam>> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(None);
    }

    let mut payload = trimmed.to_string();
    let not_used = payload.contains(PARAM_NOT_USED_MARKER);
    if not_used {
        payload = payload.replace(PARAM_NOT_USED_MARKER, "");
    }

    let (val_part, comment) = if let Some((left, right)) = payload.split_once("//") {
        (left, right.trim().to_string())
    } else {
        (payload.as_str(), String::new())
    };

    let cols: Vec<&str> = val_part.split(',').map(str::trim).collect();
    if cols.len() < 7 {
        bail!("invalid params line {}: '{}'", line_no, line);
    }

    let type_name = cols[1].to_string();
    let is_int = type_name.eq_ignore_ascii_case("int");

    Ok(Some(SpsaParam {
        name: cols[0].to_string(),
        type_name,
        is_int,
        value: cols[2]
            .parse::<f64>()
            .with_context(|| format!("invalid v at line {}", line_no))?,
        min: cols[3]
            .parse::<f64>()
            .with_context(|| format!("invalid min at line {}", line_no))?,
        max: cols[4]
            .parse::<f64>()
            .with_context(|| format!("invalid max at line {}", line_no))?,
        step: cols[5]
            .parse::<f64>()
            .with_context(|| format!("invalid step at line {}", line_no))?,
        delta: cols[6]
            .parse::<f64>()
            .with_context(|| format!("invalid delta at line {}", line_no))?,
        comment,
        not_used,
    }))
}

fn read_params(path: &Path) -> Result<Vec<SpsaParam>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut params = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if let Some(param) = parse_param_line(&line, idx + 1)? {
            params.push(param);
        }
    }
    if params.is_empty() {
        bail!("no parameters loaded from {}", path.display());
    }
    Ok(params)
}

fn write_params(path: &Path, params: &[SpsaParam]) -> Result<()> {
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let mut w = BufWriter::new(file);
    for p in params {
        let v_str = if p.is_int {
            format!("{}", p.value.round() as i64)
        } else {
            format!("{}", p.value)
        };
        let mut line = format!(
            "{},{},{},{},{},{},{}",
            p.name, p.type_name, v_str, p.min, p.max, p.step, p.delta
        );
        if !p.comment.is_empty() {
            line.push_str(" //");
            line.push_str(&p.comment);
        }
        if p.not_used {
            line.push_str(PARAM_NOT_USED_MARKER);
        }
        writeln!(w, "{line}")?;
    }
    w.flush()?;
    Ok(())
}

fn option_value_string(param: &SpsaParam, value: f64) -> String {
    if param.is_int {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.6}")
    }
}

fn clamped_value(param: &SpsaParam, raw: f64) -> f64 {
    raw.clamp(param.min, param.max)
}

fn resolve_engine_path(cli: &Cli) -> Result<PathBuf> {
    if let Some(path) = &cli.engine_path {
        return Ok(path.clone());
    }
    let release = PathBuf::from("target/release/rshogi-usi");
    if release.exists() {
        return Ok(release);
    }
    let debug = PathBuf::from("target/debug/rshogi-usi");
    if debug.exists() {
        return Ok(debug);
    }
    bail!("engine binary not found. specify --engine-path or build target/release/rshogi-usi");
}

fn apply_parameter_vector(
    engine: &mut EngineProcess,
    params: &[SpsaParam],
    values: &[f64],
) -> Result<()> {
    for (p, &v) in params.iter().zip(values.iter()) {
        if p.not_used {
            continue;
        }
        engine.set_option_if_available(&p.name, &option_value_string(p, v))?;
    }
    engine.sync_ready()?;
    Ok(())
}

fn plus_score_from_outcome(outcome: GameOutcome, plus_is_black: bool) -> f64 {
    match outcome {
        GameOutcome::Draw | GameOutcome::InProgress => 0.0,
        GameOutcome::BlackWin => {
            if plus_is_black {
                1.0
            } else {
                -1.0
            }
        }
        GameOutcome::WhiteWin => {
            if plus_is_black {
                -1.0
            } else {
                1.0
            }
        }
    }
}

fn pick_startpos<'a>(
    start_positions: &'a [ParsedPosition],
    rng: &mut impl rand::Rng,
    random: bool,
    game_index: usize,
) -> Result<&'a ParsedPosition> {
    if random {
        start_positions.choose(rng).context("no start positions available")
    } else {
        Ok(&start_positions[game_index % start_positions.len()])
    }
}

fn resolve_seeds(cli: &Cli) -> Vec<u64> {
    if let Some(seeds) = &cli.seeds {
        return seeds.clone();
    }
    if let Some(seed) = cli.seed {
        return vec![seed];
    }
    let mut rng = rand::rng();
    vec![rng.random()]
}

fn mean_and_variance(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let mean = values.iter().copied().sum::<f64>() / values.len() as f64;
    let variance = values
        .iter()
        .map(|value| {
            let diff = value - mean;
            diff * diff
        })
        .sum::<f64>()
        / values.len() as f64;
    (mean, variance)
}

fn seed_for_iteration(base_seed: u64, iteration_index: u32) -> u64 {
    let iter_term = (iteration_index as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    base_seed ^ iter_term
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();

    let cli = Cli::parse();
    if cli.games_per_iteration == 0 || cli.games_per_iteration % 2 != 0 {
        bail!("--games-per-iteration must be an even number >= 2");
    }
    if cli.iterations == 0 {
        bail!("--iterations must be >= 1");
    }
    if cli.scale <= 0.0 {
        bail!("--scale must be > 0");
    }
    if cli.a <= 0.0 || cli.c <= 0.0 {
        bail!("--a and --c must be > 0");
    }
    if cli.alpha <= 0.0 || cli.gamma <= 0.0 {
        bail!("--alpha and --gamma must be > 0");
    }
    if cli.a_offset < 0.0 {
        bail!("--a-offset must be >= 0");
    }
    let seed_values = resolve_seeds(&cli);
    if seed_values.is_empty() {
        bail!("at least one seed is required");
    }
    println!("using base seeds: {:?}", seed_values);

    let engine_path = resolve_engine_path(&cli)?;
    let engine_args = cli.engine_args.clone().unwrap_or_default();
    let mut params = read_params(&cli.params)?;
    let schedule = ScheduleConfig {
        a: cli.a,
        a_offset: cli.a_offset,
        alpha: cli.alpha,
        c: cli.c,
        gamma: cli.gamma,
        scale: cli.scale,
        mobility: cli.mobility,
    };
    let meta_path = cli.meta_file.clone().unwrap_or_else(|| default_meta_path(&cli.params));
    let (start_iteration, mut total_games) = if cli.resume {
        let meta = load_meta(&meta_path).with_context(|| {
            format!("--resume was set but metadata load failed: {}", meta_path.display())
        })?;
        if meta.format_version != META_FORMAT_VERSION {
            bail!(
                "unsupported meta format version {} in {}",
                meta.format_version,
                meta_path.display()
            );
        }
        if meta.schedule != schedule {
            eprintln!(
                "warning: schedule differs from metadata. continuing with current CLI values \
                 (meta={}, cli={:?})",
                meta_path.display(),
                schedule
            );
        }
        (meta.completed_iterations, meta.total_games)
    } else {
        (0, 0)
    };
    let end_iteration = start_iteration
        .checked_add(cli.iterations)
        .context("iteration index overflow")?;
    let aggregate_csv_path = if let Some(path) = &cli.stats_aggregate_csv {
        Some(path.clone())
    } else if seed_values.len() > 1 {
        cli.stats_csv
            .as_ref()
            .map(|path| PathBuf::from(format!("{}.aggregate.csv", path.display())))
    } else {
        None
    };
    let mut stats_csv_writer = if let Some(path) = &cli.stats_csv {
        Some(open_stats_csv_writer(path, cli.resume)?)
    } else {
        None
    };
    let mut stats_aggregate_csv_writer = if let Some(path) = aggregate_csv_path.as_deref() {
        Some(open_stats_aggregate_csv_writer(path, cli.resume)?)
    } else {
        None
    };

    let (start_positions, _) =
        load_start_positions(cli.startpos_file.as_deref(), cli.sfen.as_deref(), None, None)?;

    let base_cfg = EngineConfig {
        path: engine_path,
        args: engine_args,
        threads: cli.threads,
        hash_mb: cli.hash_mb,
        network_delay: None,
        network_delay2: None,
        minimum_thinking_time: None,
        slowmover: None,
        ponder: false,
        usi_options: Vec::new(),
    };

    let mut plus_engine = EngineProcess::spawn(&base_cfg, "plus".to_string())?;
    let mut minus_engine = EngineProcess::spawn(&base_cfg, "minus".to_string())?;

    let game_cfg = GameConfig {
        max_moves: cli.max_moves,
        timeout_margin_ms: cli.timeout_margin_ms,
        pass_rights: None,
    };
    let tc = TimeControl::new(0, 0, 0, 0, cli.byoyomi);

    for iter in start_iteration..end_iteration {
        let (a_t, c_t) = schedule_values(schedule, iter);
        let mut grad_sums = vec![0.0f64; params.len()];
        let mut seed_step_sums = Vec::with_capacity(seed_values.len());
        let mut seed_grad_scales = Vec::with_capacity(seed_values.len());
        let mut seed_plus_wins = Vec::with_capacity(seed_values.len());
        let mut seed_minus_wins = Vec::with_capacity(seed_values.len());
        let mut seed_draws = Vec::with_capacity(seed_values.len());
        let mut seed_rows = Vec::with_capacity(seed_values.len());

        for (seed_idx, base_seed) in seed_values.iter().copied().enumerate() {
            let iter_seed = seed_for_iteration(base_seed, iter);
            let mut rng = ChaCha8Rng::seed_from_u64(iter_seed);
            let shifts: Vec<f64> = params
                .iter()
                .map(|p| {
                    if p.not_used {
                        0.0
                    } else if rng.random_bool(0.5) {
                        p.step * cli.scale * c_t
                    } else {
                        -p.step * cli.scale * c_t
                    }
                })
                .collect();

            let plus_values: Vec<f64> = params
                .iter()
                .zip(shifts.iter())
                .map(|(p, s)| clamped_value(p, p.value + s))
                .collect();
            let minus_values: Vec<f64> = params
                .iter()
                .zip(shifts.iter())
                .map(|(p, s)| clamped_value(p, p.value - s))
                .collect();

            let mut active_params = 0usize;
            let mut abs_shift_sum = 0.0f64;
            for (p, &shift) in params.iter().zip(shifts.iter()) {
                if p.not_used {
                    continue;
                }
                active_params += 1;
                abs_shift_sum += shift.abs();
            }
            let avg_abs_shift = if active_params > 0 {
                abs_shift_sum / active_params as f64
            } else {
                0.0
            };

            let mut step_sum = 0.0f64;
            let mut plus_wins = 0u32;
            let mut minus_wins = 0u32;
            let mut draws = 0u32;

            for game_idx in 0..cli.games_per_iteration {
                let plus_is_black = game_idx % 2 == 0;
                if plus_is_black {
                    apply_parameter_vector(&mut plus_engine, &params, &plus_values)?;
                    apply_parameter_vector(&mut minus_engine, &params, &minus_values)?;
                } else {
                    apply_parameter_vector(&mut plus_engine, &params, &minus_values)?;
                    apply_parameter_vector(&mut minus_engine, &params, &plus_values)?;
                }
                plus_engine.new_game()?;
                minus_engine.new_game()?;

                let start_pos =
                    pick_startpos(&start_positions, &mut rng, cli.random_startpos, total_games)?;
                total_games += 1;

                let mut on_move = |_event: &MoveEvent| {};
                let result = if plus_is_black {
                    run_game(
                        &mut plus_engine,
                        &mut minus_engine,
                        start_pos,
                        tc,
                        &game_cfg,
                        total_games as u32,
                        &mut on_move,
                        None,
                    )?
                } else {
                    run_game(
                        &mut minus_engine,
                        &mut plus_engine,
                        start_pos,
                        tc,
                        &game_cfg,
                        total_games as u32,
                        &mut on_move,
                        None,
                    )?
                };

                let plus_score = plus_score_from_outcome(result.outcome, plus_is_black);
                step_sum += plus_score;
                if plus_score > 0.0 {
                    plus_wins += 1;
                } else if plus_score < 0.0 {
                    minus_wins += 1;
                } else {
                    draws += 1;
                }
                println!(
                    "iter={} seed={}/{}({}) game={}/{} plus_is_black={} outcome={} plus_score={:+.1}",
                    iter + 1,
                    seed_idx + 1,
                    seed_values.len(),
                    base_seed,
                    game_idx + 1,
                    cli.games_per_iteration,
                    plus_is_black,
                    result.outcome.label(),
                    plus_score
                );
            }

            let grad_scale = step_sum / cli.games_per_iteration as f64;
            if c_t > f64::EPSILON {
                for (idx, (p, &shift)) in params.iter().zip(shifts.iter()).enumerate() {
                    if p.not_used || p.step.abs() <= f64::EPSILON {
                        continue;
                    }
                    let direction = if shift >= 0.0 { 1.0 } else { -1.0 };
                    let grad = grad_scale * direction / (p.step.abs() * cli.scale * c_t);
                    grad_sums[idx] += grad;
                }
            }

            seed_step_sums.push(step_sum);
            seed_grad_scales.push(grad_scale);
            seed_plus_wins.push(plus_wins as f64);
            seed_minus_wins.push(minus_wins as f64);
            seed_draws.push(draws as f64);

            seed_rows.push(IterationStats {
                iteration: iter + 1,
                seed: base_seed,
                games: cli.games_per_iteration,
                plus_wins,
                minus_wins,
                draws,
                step_sum,
                grad_scale,
                a_t,
                c_t,
                active_params,
                avg_abs_shift,
                updated_params: 0,
                avg_abs_update: 0.0,
                max_abs_update: 0.0,
                total_games: 0,
            });
        }

        let grad_scale = if seed_values.is_empty() {
            0.0
        } else {
            seed_grad_scales.iter().copied().sum::<f64>() / seed_values.len() as f64
        };
        let mut updated_params = 0usize;
        let mut abs_update_sum = 0.0f64;
        let mut max_abs_update = 0.0f64;
        for (idx, p) in params.iter_mut().enumerate() {
            if p.not_used || p.step.abs() <= f64::EPSILON || c_t <= f64::EPSILON {
                continue;
            }
            let before = p.value;
            let grad = grad_sums[idx] / seed_values.len() as f64;
            let updated = clamped_value(p, p.value + a_t * p.delta * grad * cli.mobility);
            p.value = if p.is_int { updated.round() } else { updated };
            let abs_update = (p.value - before).abs();
            updated_params += 1;
            abs_update_sum += abs_update;
            if abs_update > max_abs_update {
                max_abs_update = abs_update;
            }
        }
        let avg_abs_update = if updated_params > 0 {
            abs_update_sum / updated_params as f64
        } else {
            0.0
        };
        if let Some(writer) = stats_csv_writer.as_mut() {
            for row in &mut seed_rows {
                row.updated_params = updated_params;
                row.avg_abs_update = avg_abs_update;
                row.max_abs_update = max_abs_update;
                row.total_games = total_games;
                write_stats_csv_row(writer, *row)?;
            }
            writer.flush()?;
        }

        let (step_sum_mean, step_sum_variance) = mean_and_variance(&seed_step_sums);
        let (grad_scale_mean, grad_scale_variance) = mean_and_variance(&seed_grad_scales);
        let (plus_wins_mean, plus_wins_variance) = mean_and_variance(&seed_plus_wins);
        let (minus_wins_mean, minus_wins_variance) = mean_and_variance(&seed_minus_wins);
        let (draws_mean, draws_variance) = mean_and_variance(&seed_draws);

        write_params(&cli.params, &params)?;
        let meta = ResumeMetaData {
            format_version: META_FORMAT_VERSION,
            params_file: cli.params.display().to_string(),
            completed_iterations: iter + 1,
            total_games,
            last_step_sum: step_sum_mean,
            last_grad_scale: grad_scale,
            last_a_t: a_t,
            last_c_t: c_t,
            updated_at_utc: Utc::now().to_rfc3339(),
            schedule,
        };
        save_meta(&meta_path, &meta)?;
        println!(
            "iter={} seeds={} step_sum_mean={:+.3} step_sum_var={:.6} grad_scale_mean={:+.3} \
             grad_scale_var={:.6} a_t={:.6} c_t={:.6} checkpoint={} meta={}",
            iter + 1,
            seed_values.len(),
            step_sum_mean,
            step_sum_variance,
            grad_scale_mean,
            grad_scale_variance,
            a_t,
            c_t,
            cli.params.display(),
            meta_path.display()
        );
        if let Some(writer) = stats_aggregate_csv_writer.as_mut() {
            write_stats_aggregate_csv_row(
                writer,
                AggregateIterationStats {
                    iteration: iter + 1,
                    seed_count: seed_values.len(),
                    games_per_seed: cli.games_per_iteration,
                    step_sum_mean,
                    step_sum_variance,
                    grad_scale_mean,
                    grad_scale_variance,
                    plus_wins_mean,
                    plus_wins_variance,
                    minus_wins_mean,
                    minus_wins_variance,
                    draws_mean,
                    draws_variance,
                    total_games,
                },
            )?;
            writer.flush()?;
        }
    }

    Ok(())
}
