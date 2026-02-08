use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Parser;
use rand::prelude::IndexedRandom;
use rand::Rng;
use tools::selfplay::game::{run_game, GameConfig, MoveEvent};
use tools::selfplay::time_control::TimeControl;
use tools::selfplay::{
    load_start_positions, EngineConfig, EngineProcess, GameOutcome, ParsedPosition,
};

const PARAM_NOT_USED_MARKER: &str = "[[NOT USED]]";

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

    let engine_path = resolve_engine_path(&cli)?;
    let engine_args = cli.engine_args.clone().unwrap_or_default();
    let mut params = read_params(&cli.params)?;

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

    let mut rng = rand::rng();
    let mut total_games = 0usize;

    for iter in 0..cli.iterations {
        let shifts: Vec<f64> = params
            .iter()
            .map(|p| {
                if p.not_used {
                    0.0
                } else if rng.random_bool(0.5) {
                    p.step
                } else {
                    -p.step
                }
            })
            .collect();

        let plus_values: Vec<f64> = params
            .iter()
            .zip(shifts.iter())
            .map(|(p, s)| clamped_value(p, p.value + s * cli.scale))
            .collect();
        let minus_values: Vec<f64> = params
            .iter()
            .zip(shifts.iter())
            .map(|(p, s)| clamped_value(p, p.value - s * cli.scale))
            .collect();

        let mut step_sum = 0.0f64;

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
            println!(
                "iter={} game={}/{} plus_is_black={} outcome={} plus_score={:+.1}",
                iter + 1,
                game_idx + 1,
                cli.games_per_iteration,
                plus_is_black,
                result.outcome.label(),
                plus_score
            );
        }

        let grad_scale = step_sum / cli.games_per_iteration as f64;
        for (p, &shift) in params.iter_mut().zip(shifts.iter()) {
            if p.not_used {
                continue;
            }
            let updated = clamped_value(p, p.value + shift * grad_scale * p.delta * cli.mobility);
            p.value = if p.is_int { updated.round() } else { updated };
        }

        write_params(&cli.params, &params)?;
        println!(
            "iter={} step_sum={:+.3} grad_scale={:+.3} checkpoint={}",
            iter + 1,
            step_sum,
            grad_scale,
            cli.params.display()
        );
    }

    Ok(())
}
