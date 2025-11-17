use chrono::Local;
use serde_json::json;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::{SearchLimits, SearchLimitsBuilder};
use engine_core::shogi::{Color, Move, Position};
use engine_core::shogihome_basic::{
    BasicEngine as BasicOpponent, RepetitionTable, ShogihomeBasicStyle,
};
use engine_core::usi::{create_position, move_to_usi, position_to_sfen};
use serde::Serialize;
use tools::kif_export::convert_jsonl_to_kif;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Selfplay harness: main engine (Black) vs ShogiHome basic engine (White)"
)]
struct Cli {
    /// Number of games to run
    #[arg(long, default_value_t = 10)]
    games: u32,

    /// Maximum plies per game before declaring a draw
    #[arg(long, default_value_t = 512)]
    max_moves: u32,

    /// Fixed thinking time per Black move in milliseconds
    #[arg(long, default_value_t = 1000)]
    think_ms: u64,

    /// Threads for the main engine
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Depth for the ShogiHome basic engine search
    #[arg(long, default_value_t = 2)]
    basic_depth: u8,

    /// Enable random noise in the ShogiHome basic engine evaluation
    #[arg(long, default_value_t = false)]
    basic_noise: bool,

    /// Optional RNG seed for the ShogiHome basic engine
    #[arg(long)]
    basic_seed: Option<u64>,

    /// Style preset for the ShogiHome basic engine (static-rook, ranging-rook, random)
    #[arg(long, default_value = "static-rook", value_parser = parse_basic_style)]
    basic_style: ShogihomeBasicStyle,

    /// Engine type for the main engine (enhanced, enhanced-nnue, nnue, material)
    #[arg(long, default_value = "enhanced", value_parser = parse_engine_type)]
    engine_type: EngineType,

    /// Optional file that lists starting positions (USI position commands per line)
    #[arg(long)]
    startpos_file: Option<PathBuf>,

    /// Output path template (optional)
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Serialize)]
struct MoveLog {
    game_id: u32,
    ply: u32,
    side_to_move: char,
    sfen_before: String,
    move_usi: String,
    engine: &'static str,
    main_eval: Option<MainEvalLog>,
    basic_eval: Option<BasicEvalLog>,
    result: Option<String>,
}

#[derive(Serialize)]
struct MainEvalLog {
    score_cp: Option<i32>,
    depth: Option<u32>,
    seldepth: Option<u32>,
    nodes: Option<u64>,
    pv: Option<Vec<String>>,
}

#[derive(Serialize)]
struct BasicEvalLog {
    score: i32,
    style: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GameOutcome {
    InProgress,
    BlackWin,
    WhiteWin,
    Draw,
}

impl GameOutcome {
    fn label(self) -> &'static str {
        match self {
            GameOutcome::InProgress => "in_progress",
            GameOutcome::BlackWin => "black_win",
            GameOutcome::WhiteWin => "white_win",
            GameOutcome::Draw => "draw",
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut engine = Engine::new(cli.engine_type);
    engine.set_threads(cli.threads);
    let mut basic = BasicOpponent::new(cli.basic_style);
    basic.enable_noise(cli.basic_noise);
    if let Some(seed) = cli.basic_seed {
        basic.set_seed(seed);
    }

    let starts = load_start_positions(cli.startpos_file.as_deref())?;
    let base_out = cli.out.clone().unwrap_or_else(|| default_output_base(&cli));
    let timestamp = Local::now();
    let timestamp_prefix = timestamp.format("%Y%m%d-%H%M%S").to_string();
    let timestamp_iso = timestamp.to_rfc3339();
    let final_out = resolve_output_path(&base_out, &timestamp_prefix);
    println!("selfplay_basic: writing log to {}", final_out.display());
    let meta_start_pos = starts.first().cloned().unwrap_or_else(Position::startpos);
    let mut writer = prepare_writer(&final_out)?;
    write_metadata(&mut writer, &cli, &timestamp_iso, &base_out, &final_out, &meta_start_pos)?;

    for game_idx in 0..cli.games {
        let mut pos = starts[(game_idx as usize) % starts.len()].clone();
        let mut outcome = GameOutcome::InProgress;

        for ply_idx in 0..cli.max_moves {
            let side = pos.side_to_move;
            let sfen_before = position_to_sfen(&pos);

            if side == Color::Black {
                let (best_move, eval) = search_main_move(&mut engine, &pos, cli.think_ms)?;
                let mut move_record = if let Some(mv) = best_move {
                    pos.do_move(mv);
                    MoveLog::main(
                        game_idx + 1,
                        ply_idx + 1,
                        side,
                        sfen_before,
                        move_to_usi(&mv),
                        eval,
                    )
                } else {
                    outcome = GameOutcome::WhiteWin;
                    MoveLog::main(
                        game_idx + 1,
                        ply_idx + 1,
                        side,
                        sfen_before,
                        "resign".to_string(),
                        eval,
                    )
                };
                if outcome != GameOutcome::InProgress || ply_idx + 1 == cli.max_moves {
                    if outcome == GameOutcome::InProgress {
                        outcome = GameOutcome::Draw;
                    }
                    move_record.result = Some(outcome.label().to_string());
                }
                serde_json::to_writer(&mut writer, &move_record)?;
                writer.write_all(b"\n")?;
                writer.flush()?;
                if outcome != GameOutcome::InProgress {
                    break;
                }
            } else {
                let rep = RepetitionTable::from_position(&pos);
                let basic_result = basic
                    .search(&pos, cli.basic_depth, Some(&rep))
                    .map_err(|e| anyhow!("basic engine search failed: {e}"))?;
                let mut move_record = if let Some(mv) = basic_result.best_move {
                    let move_str = move_to_usi(&mv);
                    let log = MoveLog::basic(
                        game_idx + 1,
                        ply_idx + 1,
                        side,
                        sfen_before,
                        move_str.clone(),
                        basic_result.score,
                        cli.basic_style,
                    );
                    pos.do_move(mv);
                    log
                } else {
                    outcome = GameOutcome::BlackWin;
                    MoveLog::basic(
                        game_idx + 1,
                        ply_idx + 1,
                        side,
                        sfen_before,
                        "resign".to_string(),
                        basic_result.score,
                        cli.basic_style,
                    )
                };
                if outcome != GameOutcome::InProgress || ply_idx + 1 == cli.max_moves {
                    if outcome == GameOutcome::InProgress {
                        outcome = GameOutcome::Draw;
                    }
                    move_record.result = Some(outcome.label().to_string());
                }
                serde_json::to_writer(&mut writer, &move_record)?;
                writer.write_all(b"\n")?;
                writer.flush()?;
                if outcome != GameOutcome::InProgress {
                    break;
                }
            }
        }
    }

    writer.flush()?;

    // generate KIF automatically
    let kif_path = default_kif_path(&final_out);
    if let Err(err) = convert_jsonl_to_kif(&final_out, &kif_path) {
        eprintln!("failed to create KIF: {}", err);
    } else {
        println!("kif written to {}", kif_path.display());
    }

    Ok(())
}

fn parse_basic_style(value: &str) -> Result<ShogihomeBasicStyle, String> {
    match value.to_ascii_lowercase().as_str() {
        "static-rook" => Ok(ShogihomeBasicStyle::StaticRookV1),
        "ranging-rook" => Ok(ShogihomeBasicStyle::RangingRookV1),
        "random" => Ok(ShogihomeBasicStyle::Random),
        other => Err(format!(
            "invalid basic style '{other}'. expected static-rook, ranging-rook, or random"
        )),
    }
}

fn parse_engine_type(value: &str) -> Result<EngineType, String> {
    match value.to_ascii_lowercase().as_str() {
        "enhanced-nnue" => Ok(EngineType::EnhancedNnue),
        "enhanced" => Ok(EngineType::Enhanced),
        "nnue" => Ok(EngineType::Nnue),
        "material" => Ok(EngineType::Material),
        other => Err(format!(
            "invalid engine type '{other}'. expected enhanced-nnue, enhanced, nnue, or material"
        )),
    }
}

fn prepare_writer(path: &Path) -> Result<BufWriter<File>> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create output directory {}", parent.display())
            })?;
        }
    }
    let file = File::create(path).with_context(|| format!("failed to open {}", path.display()))?;
    Ok(BufWriter::new(file))
}

fn load_start_positions(path: Option<&Path>) -> Result<Vec<Position>> {
    if let Some(path) = path {
        let file = File::open(path)
            .with_context(|| format!("failed to open start position file {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut positions = Vec::new();
        for (idx, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (startpos, sfen, moves) = parse_position_line(trimmed).with_context(|| {
                format!("invalid position syntax on line {}: {}", idx + 1, trimmed)
            })?;
            let pos = create_position(startpos, sfen.as_deref(), &moves).with_context(|| {
                format!("failed to create position from line {}: {}", idx + 1, trimmed)
            })?;
            positions.push(pos);
        }
        if positions.is_empty() {
            anyhow::bail!("no usable positions found in {}", path.display());
        }
        Ok(positions)
    } else {
        Ok(vec![Position::startpos()])
    }
}

fn parse_position_line(line: &str) -> Result<(bool, Option<String>, Vec<String>)> {
    let mut tokens = line.split_whitespace().peekable();
    if tokens.peek().is_some_and(|tok| *tok == "position") {
        tokens.next();
    }
    match tokens.next() {
        Some("startpos") => {
            let moves = parse_moves(tokens)?;
            Ok((true, None, moves))
        }
        Some("sfen") => {
            let mut sfen_tokens = Vec::new();
            while let Some(token) = tokens.peek() {
                if *token == "moves" {
                    break;
                }
                sfen_tokens.push(tokens.next().unwrap().to_string());
            }
            if sfen_tokens.is_empty() {
                return Err(anyhow!("missing SFEN payload"));
            }
            let moves = parse_moves(tokens)?;
            Ok((false, Some(sfen_tokens.join(" ")), moves))
        }
        other => Err(anyhow!("expected 'startpos' or 'sfen' after 'position', got {:?}", other)),
    }
}

fn parse_moves<'a, I>(iter: I) -> Result<Vec<String>>
where
    I: Iterator<Item = &'a str>,
{
    let mut iter = iter.peekable();
    match iter.peek() {
        Some(&"moves") => {
            iter.next();
            Ok(iter.map(|mv| mv.to_string()).collect())
        }
        Some(other) => Err(anyhow!("expected 'moves' keyword before move list, got '{other}'")),
        None => Ok(Vec::new()),
    }
}

fn build_limits(ms: u64) -> SearchLimits {
    SearchLimitsBuilder::default().fixed_time_ms(ms).build()
}

fn search_main_move(
    engine: &mut Engine,
    pos: &Position,
    think_ms: u64,
) -> Result<(Option<Move>, Option<MainEvalLog>)> {
    let mut scratch = pos.clone();
    let result = engine.search(&mut scratch, build_limits(think_ms));
    let final_best = engine.choose_final_bestmove(pos, None);
    let best_move = final_best.best_move.or(result.best_move);
    let pv = if !final_best.pv.is_empty() {
        Some(final_best.pv.iter().map(move_to_usi).collect::<Vec<_>>())
    } else if !result.stats.pv.is_empty() {
        Some(result.stats.pv.iter().map(move_to_usi).collect::<Vec<_>>())
    } else {
        None
    };
    let eval = Some(MainEvalLog {
        score_cp: Some(result.score),
        depth: Some(result.depth),
        seldepth: Some(result.seldepth),
        nodes: Some(result.nodes),
        pv,
    });
    Ok((best_move, eval))
}

impl MoveLog {
    fn main(
        game_id: u32,
        ply: u32,
        side: Color,
        sfen_before: String,
        move_usi: String,
        eval: Option<MainEvalLog>,
    ) -> Self {
        Self {
            game_id,
            ply,
            side_to_move: side_label(side),
            sfen_before,
            move_usi,
            engine: "main",
            main_eval: eval,
            basic_eval: None,
            result: None,
        }
    }

    fn basic(
        game_id: u32,
        ply: u32,
        side: Color,
        sfen_before: String,
        move_usi: String,
        score: i32,
        style: ShogihomeBasicStyle,
    ) -> Self {
        Self {
            game_id,
            ply,
            side_to_move: side_label(side),
            sfen_before,
            move_usi,
            engine: "basic",
            main_eval: None,
            basic_eval: Some(BasicEvalLog {
                score,
                style: style_label(style),
            }),
            result: None,
        }
    }
}

fn side_label(color: Color) -> char {
    if color == Color::Black {
        'b'
    } else {
        'w'
    }
}

fn style_label(style: ShogihomeBasicStyle) -> &'static str {
    match style {
        ShogihomeBasicStyle::StaticRookV1 => "static-rook",
        ShogihomeBasicStyle::RangingRookV1 => "ranging-rook",
        ShogihomeBasicStyle::Random => "random",
    }
}

fn engine_type_label(engine_type: EngineType) -> &'static str {
    match engine_type {
        EngineType::Material => "material",
        EngineType::Nnue => "nnue",
        EngineType::Enhanced => "enhanced",
        EngineType::EnhancedNnue => "enhanced-nnue",
    }
}

fn default_output_base(cli: &Cli) -> PathBuf {
    let dir = PathBuf::from("runs/selfplay-basic");
    let file = format!(
        "selfplay_{engine}_{threads}t_{style}_d{depth}_{think}ms.jsonl",
        engine = engine_type_label(cli.engine_type),
        threads = cli.threads,
        style = style_label(cli.basic_style),
        depth = cli.basic_depth,
        think = cli.think_ms,
    );
    dir.join(file)
}

fn default_kif_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.kif"))
}

fn resolve_output_path(out: &Path, timestamp: &str) -> PathBuf {
    let default_name = OsString::from("selfplay.jsonl");
    let (mut dir, base) = match std::fs::metadata(out) {
        Ok(meta) if meta.is_dir() => (out.to_path_buf(), default_name.clone()),
        _ => match out.file_name() {
            Some(name) => {
                let parent =
                    out.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
                (
                    if parent.as_os_str().is_empty() {
                        PathBuf::from(".")
                    } else {
                        parent
                    },
                    name.to_os_string(),
                )
            }
            None => (out.to_path_buf(), default_name.clone()),
        },
    };
    let file_name = if base.is_empty() { default_name } else { base };
    let mut new_name = OsString::from(format!("{timestamp}-"));
    new_name.push(&file_name);
    dir.push(new_name);
    dir
}

fn write_metadata(
    writer: &mut BufWriter<File>,
    cli: &Cli,
    timestamp_iso: &str,
    base_path: &Path,
    final_path: &Path,
    start_position: &Position,
) -> Result<()> {
    let command = std::env::args().collect::<Vec<_>>().join(" ");
    let start_sfen = position_to_sfen(start_position);
    let meta = json!({
        "type": "meta",
        "timestamp": timestamp_iso,
        "output": final_path.display().to_string(),
        "output_template": base_path.display().to_string(),
        "command": command,
        "settings": {
            "games": cli.games,
            "max_moves": cli.max_moves,
            "think_ms": cli.think_ms,
            "threads": cli.threads,
            "basic_depth": cli.basic_depth,
            "basic_noise": cli.basic_noise,
            "basic_seed": cli.basic_seed,
            "basic_style": style_label(cli.basic_style),
            "engine_type": engine_type_label(cli.engine_type),
            "startpos_file": cli.startpos_file.as_ref().map(|p| p.display().to_string()),
            "output_base": base_path.display().to_string(),
        },
        "start_sfen": start_sfen,
        "engine_names": {
            "black": format!("main ({})", engine_type_label(cli.engine_type)),
            "white": format!("basic ({})", style_label(cli.basic_style)),
        },
        "think_ms": {
            "black": cli.think_ms,
            "white": 0
        },
    });
    serde_json::to_writer(&mut *writer, &meta)?;
    writer.write_all(b"\n")?;
    Ok(())
}
