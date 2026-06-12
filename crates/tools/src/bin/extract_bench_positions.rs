//! 棋譜から教師ラベル評価用のベンチ局面を抽出する。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use rand::SeedableRng;
use rand::seq::SliceRandom;
use rand_chacha::ChaCha8Rng;
use rshogi_core::movegen::{MoveList, generate_legal};
use rshogi_core::position::Position;
use rshogi_core::types::{
    Color, EnteringKingRule, File as ShogiFile, Move, PieceType, Rank, Square,
};
use serde::{Deserialize, Serialize};
use tools::common::dedup::collect_input_paths;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "CSA/selfplay JSONL から教師ラベル評価用ベンチ局面を抽出"
)]
struct Cli {
    /// floodgate CSA 棋譜ディレクトリまたは glob。複数指定可。
    #[arg(long)]
    csa_dir: Vec<String>,

    /// selfplay JSONL glob。複数指定可。
    #[arg(long)]
    jsonl: Vec<String>,

    /// 出力ディレクトリ。
    #[arg(long)]
    out_dir: PathBuf,

    /// floodgate の両対局者に要求する最小レート。不明レートは除外。
    #[arg(long, default_value_t = 3000)]
    min_rating: u32,

    /// label_bench の層化セルあたり採択数。
    #[arg(long, default_value_t = 200)]
    per_cell: usize,

    /// 入玉オーバーサンプルの最大局面数。
    #[arg(long, default_value_t = 50_000)]
    nyugyoku_max: usize,

    /// startpos 出力に許す絶対評価値上限。
    #[arg(long, default_value_t = 150)]
    startpos_eval_abs_max: i32,

    /// startpos 出力の中心 ply。
    #[arg(long, default_value_t = 100)]
    startpos_ply: u32,

    /// startpos 出力の ply 窓幅。
    #[arg(long, default_value_t = 4)]
    startpos_window: u32,

    /// 決定的サンプリング用 seed。
    #[arg(long, default_value_t = 1)]
    seed: u64,
}

#[derive(Debug, Clone, Serialize)]
struct BenchRecord {
    sfen: String,
    ply: u32,
    eval_cp_black: Option<i32>,
    stm: char,
    progress_band: String,
    eval_band: String,
    nyugyoku: String,
    black_points: u32,
    white_points: u32,
    in_check: bool,
    source: Source,
    game_id: String,
    end_kind: String,
    result: String,
    min_rating: Option<u32>,
}

#[derive(Debug, Clone)]
struct Candidate {
    record: BenchRecord,
    usi_line: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Floodgate,
    Selfplay,
}

#[derive(Debug, Default, Serialize)]
struct Stats {
    games_by_source: BTreeMap<String, u64>,
    skipped_by_reason: BTreeMap<String, u64>,
    end_kind: BTreeMap<String, u64>,
    population_by_cell: BTreeMap<String, u64>,
    accepted_by_cell: BTreeMap<String, u64>,
    source_positions: BTreeMap<String, u64>,
    sign_validation: SignValidation,
    total_positions: u64,
    label_bench: u64,
    nyugyoku_positions: u64,
    startpos_positions: u64,
}

#[derive(Debug, Default, Serialize)]
struct SignValidation {
    checked_toryo_games: u64,
    resign_side_negative: u64,
    contradictory: u64,
    samples: Vec<SignSample>,
}

#[derive(Debug, Serialize)]
struct SignSample {
    game_id: String,
    file: String,
    resign_side: char,
    last_eval_raw: i32,
    normalized_black: i32,
    ok: bool,
}

#[derive(Debug)]
struct ExtractedGame {
    candidates: Vec<Candidate>,
    nyugyoku_candidates: Vec<Candidate>,
    startpos_candidate: Option<Candidate>,
    end_kind: String,
}

#[derive(Debug, Clone)]
struct CsaMoveLine {
    raw: String,
    eval_raw: Option<i32>,
}

#[derive(Debug)]
struct CsaGame {
    moves: Vec<CsaMoveLine>,
    end_kind: String,
    black_rate: Option<u32>,
    white_rate: Option<u32>,
    non_hirate: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum JsonlEntry {
    Move(JsonlMove),
    Result(JsonlResult),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct JsonlMove {
    game_id: u32,
    ply: u32,
    side_to_move: char,
    sfen_before: String,
    move_usi: String,
    #[serde(default)]
    eval: Option<JsonlEval>,
}

#[derive(Debug, Deserialize)]
struct JsonlEval {
    score_cp: Option<i32>,
    score_mate: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct JsonlResult {
    game_id: u32,
    outcome: JsonlOutcome,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JsonlOutcome {
    BlackWin,
    WhiteWin,
    Draw,
    InProgress,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    fs::create_dir_all(&cli.out_dir)
        .with_context(|| format!("出力ディレクトリを作成できません: {}", cli.out_dir.display()))?;

    let mut stats = Stats::default();
    let mut all = Vec::new();
    let mut nyugyoku = Vec::new();
    let mut startpos = Vec::new();

    for path in collect_csa_paths(&cli.csa_dir)? {
        match process_csa_file(&path, &cli, &mut stats) {
            Ok(Some(game)) => push_game(game, &mut all, &mut nyugyoku, &mut startpos, &mut stats),
            Ok(None) => {}
            Err(err) => {
                add_count(&mut stats.skipped_by_reason, "csa_parse_error");
                eprintln!("CSA skip: {}: {err:#}", path.display());
            }
        }
    }

    for path in collect_jsonl_paths(&cli.jsonl)? {
        let games = process_jsonl_file(&path, &cli, &mut stats)
            .with_context(|| format!("JSONL 処理に失敗しました: {}", path.display()))?;
        for game in games {
            push_game(game, &mut all, &mut nyugyoku, &mut startpos, &mut stats);
        }
    }

    let mut rng = ChaCha8Rng::seed_from_u64(cli.seed);
    let sampled = stratified_sample(&all, cli.per_cell, &mut rng, &mut stats);
    let sampled_nyugyoku = sample_nyugyoku(nyugyoku, cli.nyugyoku_max, &mut rng);
    let sampled_startpos = dedup_startpos(startpos);

    stats.label_bench = sampled.len() as u64;
    stats.nyugyoku_positions = sampled_nyugyoku.len() as u64;
    stats.startpos_positions = sampled_startpos.len() as u64;

    write_jsonl(&cli.out_dir.join("label_bench.jsonl"), sampled.iter().map(|c| &c.record))?;
    write_jsonl(
        &cli.out_dir.join("label_bench_nyugyoku.jsonl"),
        sampled_nyugyoku.iter().map(|c| &c.record),
    )?;
    write_startpos(&cli.out_dir.join("startpos_ply100_balanced.txt"), &sampled_startpos)?;
    write_stats(&cli.out_dir.join("stats.json"), &stats)?;

    Ok(())
}

fn push_game(
    game: ExtractedGame,
    all: &mut Vec<Candidate>,
    nyugyoku: &mut Vec<Candidate>,
    startpos: &mut Vec<Candidate>,
    stats: &mut Stats,
) {
    add_count(&mut stats.end_kind, &game.end_kind);
    all.extend(game.candidates);
    nyugyoku.extend(game.nyugyoku_candidates);
    if let Some(candidate) = game.startpos_candidate {
        startpos.push(candidate);
    }
}

fn collect_csa_paths(inputs: &[String]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for input in inputs {
        let path = Path::new(input);
        if path.is_dir() {
            for entry in walkdir::WalkDir::new(path) {
                let entry = entry?;
                if entry.file_type().is_file() && is_csa_path(entry.path()) {
                    paths.push(entry.path().to_path_buf());
                }
            }
        } else {
            paths.extend(glob::glob(input)?.filter_map(Result::ok).filter(|p| is_csa_path(p)));
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_jsonl_paths(inputs: &[String]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for input in inputs {
        let mut collected = collect_input_paths(Some(input), None, "*.jsonl")?;
        paths.append(&mut collected);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn is_csa_path(path: &Path) -> bool {
    matches!(path.extension().and_then(|s| s.to_str()), Some("csa") | Some("CSA"))
}

fn process_csa_file(path: &Path, cli: &Cli, stats: &mut Stats) -> Result<Option<ExtractedGame>> {
    let game = parse_csa(path)?;
    if game.non_hirate {
        add_count(&mut stats.skipped_by_reason, "non_hirate");
        return Ok(None);
    }
    let min_rating = match (game.black_rate, game.white_rate) {
        (Some(b), Some(w)) => b.min(w),
        _ => {
            add_count(&mut stats.skipped_by_reason, "unknown_rating");
            return Ok(None);
        }
    };
    if min_rating < cli.min_rating {
        add_count(&mut stats.skipped_by_reason, "low_rating");
        return Ok(None);
    }

    let game_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
    let mut pos = Position::new();
    pos.set_hirate();
    let mut usi_moves = Vec::new();
    let mut candidates = Vec::new();
    let mut entered_any = false;
    let mut startpos_candidate = None;
    let mut last_eval = None;
    let mut last_side = None;

    for mv in &game.moves {
        let side = csa_side(&mv.raw)?;
        let eval_cp_black = mv.eval_raw.map(|v| normalize_csa_eval(v, side));
        last_eval = mv.eval_raw;
        last_side = Some(side);
        let record = make_record(
            &pos,
            pos.game_ply() as u32,
            eval_cp_black,
            Source::Floodgate,
            &game_id,
            &game.end_kind,
            result_from_csa(&game.end_kind, side),
            Some(min_rating),
        );
        entered_any |= record.nyugyoku != "none";
        let usi_line = usi_line(&usi_moves);
        let candidate = Candidate { record, usi_line };
        update_population(&candidate.record, stats);
        maybe_set_startpos(&candidate, cli, &mut startpos_candidate);
        candidates.push(candidate);

        let core_move = csa_to_legal_move(&pos, &mv.raw)?;
        usi_moves.push(core_move.to_usi());
        let gives_check = pos.gives_check(core_move);
        pos.do_move(core_move, gives_check);
    }

    add_count(&mut stats.games_by_source, "floodgate");
    update_sign_validation(path, &game_id, &game, last_eval, last_side, stats);
    let nyugyoku_candidates = if game.end_kind == "%KACHI" && entered_any {
        candidates.clone()
    } else {
        Vec::new()
    };
    Ok(Some(ExtractedGame {
        candidates,
        nyugyoku_candidates,
        startpos_candidate,
        end_kind: game.end_kind,
    }))
}

fn parse_csa(path: &Path) -> Result<CsaGame> {
    let file = File::open(path).with_context(|| format!("CSA を開けません: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut moves: Vec<CsaMoveLine> = Vec::new();
    let mut pending_eval: Option<i32> = None;
    let mut black_rate = None;
    let mut white_rate = None;
    let mut end_kind = "unknown".to_string();
    let mut non_hirate = false;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("'black_rate:") {
            black_rate = parse_rate_comment(trimmed);
        } else if trimmed.starts_with("'white_rate:") {
            white_rate = parse_rate_comment(trimmed);
        } else if let Some(cp) = parse_eval_comment(trimmed) {
            if let Some(last) = moves.last_mut() {
                last.eval_raw = Some(cp);
            } else {
                pending_eval = Some(cp);
            }
        } else if is_csa_move(trimmed) {
            moves.push(CsaMoveLine {
                raw: trimmed[..7].to_string(),
                eval_raw: pending_eval.take(),
            });
        } else if trimmed.starts_with('%') {
            end_kind = trimmed.split(',').next().unwrap_or(trimmed).to_string();
        } else if trimmed.starts_with("P+") || trimmed.starts_with("P-") {
            non_hirate = true;
        }
    }

    Ok(CsaGame {
        moves,
        end_kind,
        black_rate,
        white_rate,
        non_hirate,
    })
}

fn process_jsonl_file(path: &Path, cli: &Cli, stats: &mut Stats) -> Result<Vec<ExtractedGame>> {
    let file =
        File::open(path).with_context(|| format!("JSONL を開けません: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut pending: HashMap<u32, Vec<JsonlMove>> = HashMap::new();
    let mut results = HashMap::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<JsonlEntry>(&line) {
            Ok(JsonlEntry::Move(mv)) => pending.entry(mv.game_id).or_default().push(mv),
            Ok(JsonlEntry::Result(result)) => {
                results.insert(result.game_id, result);
            }
            Ok(JsonlEntry::Other) => {}
            Err(_) => add_count(&mut stats.skipped_by_reason, "jsonl_parse_error"),
        }
    }

    let mut games = Vec::new();
    for (game_id, moves) in pending {
        let Some(result) = results.get(&game_id) else {
            add_count(&mut stats.skipped_by_reason, "jsonl_orphan_game");
            continue;
        };
        if result.error || result.outcome == JsonlOutcome::InProgress {
            add_count(&mut stats.skipped_by_reason, "jsonl_error_or_in_progress");
            continue;
        }
        let game = convert_jsonl_game(game_id, moves, result, path, cli, stats)?;
        games.push(game);
        add_count(&mut stats.games_by_source, "selfplay");
    }
    Ok(games)
}

fn convert_jsonl_game(
    game_id: u32,
    mut moves: Vec<JsonlMove>,
    result: &JsonlResult,
    path: &Path,
    cli: &Cli,
    stats: &mut Stats,
) -> Result<ExtractedGame> {
    moves.sort_by_key(|m| m.ply);
    let game_id_str = format!("{}:{game_id}", path.display());
    let end_kind = result.reason.clone().unwrap_or_else(|| "result".to_string());
    let result_label = match result.outcome {
        JsonlOutcome::BlackWin => "black_win",
        JsonlOutcome::WhiteWin => "white_win",
        JsonlOutcome::Draw => "draw",
        JsonlOutcome::InProgress => "in_progress",
    }
    .to_string();

    let mut candidates = Vec::new();
    let mut startpos_candidate = None;
    for mv in moves {
        if is_terminal_move(&mv.move_usi) {
            continue;
        }
        let mut pos = Position::new();
        pos.set_sfen(&mv.sfen_before)
            .map_err(|e| anyhow!("SFEN parse error: {e:?}: {}", mv.sfen_before))?;
        if color_label(pos.side_to_move()) != mv.side_to_move {
            add_count(&mut stats.skipped_by_reason, "jsonl_side_mismatch");
            continue;
        }
        let eval_cp_black = score_from_eval(mv.eval.as_ref()).map(|cp| {
            if pos.side_to_move() == Color::Black {
                cp
            } else {
                -cp
            }
        });
        let record = make_record(
            &pos,
            mv.ply,
            eval_cp_black,
            Source::Selfplay,
            &game_id_str,
            &end_kind,
            result_label.clone(),
            None,
        );
        let usi_line = if mv.sfen_before.starts_with("lnsgkgsnl/") {
            "startpos".to_string()
        } else {
            format!("sfen {}", mv.sfen_before)
        };
        let candidate = Candidate { record, usi_line };
        update_population(&candidate.record, stats);
        maybe_set_startpos(&candidate, cli, &mut startpos_candidate);
        candidates.push(candidate);
    }

    Ok(ExtractedGame {
        candidates,
        nyugyoku_candidates: Vec::new(),
        startpos_candidate,
        end_kind,
    })
}

fn make_record(
    pos: &Position,
    ply: u32,
    eval_cp_black: Option<i32>,
    source: Source,
    game_id: &str,
    end_kind: &str,
    result: String,
    min_rating: Option<u32>,
) -> BenchRecord {
    let black_points = entering_points(pos, Color::Black);
    let white_points = entering_points(pos, Color::White);
    let black_entered = king_entered(pos, Color::Black);
    let white_entered = king_entered(pos, Color::White);
    let nyugyoku = match (black_entered, white_entered) {
        (true, true) => "both_entered",
        (true, false) => "black_entered",
        (false, true) => "white_entered",
        (false, false) => "none",
    }
    .to_string();
    let _declaration_probe = pos.declaration_win(EnteringKingRule::Point27);

    BenchRecord {
        sfen: pos.to_sfen(),
        ply,
        eval_cp_black,
        stm: color_label(pos.side_to_move()),
        progress_band: progress_band(ply).to_string(),
        eval_band: eval_band(eval_cp_black).to_string(),
        nyugyoku,
        black_points,
        white_points,
        in_check: pos.in_check(),
        source,
        game_id: game_id.to_string(),
        end_kind: end_kind.to_string(),
        result,
        min_rating,
    }
}

fn csa_to_legal_move(pos: &Position, raw: &str) -> Result<Move> {
    let spec = CsaMoveSpec::parse(raw)?;
    let mut list = MoveList::new();
    generate_legal(pos, &mut list);
    for mv in list.iter().copied() {
        if csa_spec_matches(pos, mv, &spec) {
            return Ok(mv);
        }
    }
    bail!("合法手に一致しない CSA 指し手: {raw}, sfen={}", pos.to_sfen())
}

#[derive(Debug)]
struct CsaMoveSpec {
    side: Color,
    from: Option<Square>,
    to: Square,
    piece_type_after: PieceType,
}

impl CsaMoveSpec {
    fn parse(raw: &str) -> Result<Self> {
        if !is_csa_move(raw) {
            bail!("CSA 指し手形式ではありません: {raw}");
        }
        let side = csa_side(raw)?;
        let bytes = raw.as_bytes();
        let from = if &raw[1..3] == "00" {
            None
        } else {
            Some(csa_square(bytes[1], bytes[2])?)
        };
        let to = csa_square(bytes[3], bytes[4])?;
        let piece_type_after = csa_piece_type(&raw[5..7])?;
        Ok(Self {
            side,
            from,
            to,
            piece_type_after,
        })
    }
}

fn csa_spec_matches(pos: &Position, mv: Move, spec: &CsaMoveSpec) -> bool {
    if pos.side_to_move() != spec.side || mv.to() != spec.to {
        return false;
    }
    if let Some(from) = spec.from {
        if mv.is_drop() || mv.from() != from {
            return false;
        }
        let pt = pos.piece_on(from).piece_type();
        let after = if mv.is_promote() {
            match pt.promote() {
                Some(promoted) => promoted,
                None => return false,
            }
        } else {
            pt
        };
        after == spec.piece_type_after
    } else {
        mv.is_drop() && mv.drop_piece_type() == spec.piece_type_after
    }
}

fn csa_square(file: u8, rank: u8) -> Result<Square> {
    let file = file.checked_sub(b'1').filter(|v| *v < 9).context("bad CSA file")?;
    let rank = rank.checked_sub(b'1').filter(|v| *v < 9).context("bad CSA rank")?;
    let file = ShogiFile::from_u8(file).context("bad file")?;
    let rank = Rank::from_u8(rank).context("bad rank")?;
    Ok(Square::new(file, rank))
}

fn csa_piece_type(code: &str) -> Result<PieceType> {
    let pt = match code {
        "FU" => PieceType::Pawn,
        "KY" => PieceType::Lance,
        "KE" => PieceType::Knight,
        "GI" => PieceType::Silver,
        "KI" => PieceType::Gold,
        "KA" => PieceType::Bishop,
        "HI" => PieceType::Rook,
        "OU" => PieceType::King,
        "TO" => PieceType::ProPawn,
        "NY" => PieceType::ProLance,
        "NK" => PieceType::ProKnight,
        "NG" => PieceType::ProSilver,
        "UM" => PieceType::Horse,
        "RY" => PieceType::Dragon,
        _ => bail!("未知の CSA 駒コード: {code}"),
    };
    Ok(pt)
}

fn entering_points(pos: &Position, color: Color) -> u32 {
    let enemy_field = enemy_field_ranks(color);
    let mut score = 0;
    for sq in (pos.pieces_c(color) & enemy_field).iter() {
        let pt = pos.piece_on(sq).piece_type();
        if pt == PieceType::King {
            continue;
        }
        score += piece_point(pt);
    }
    let hand = pos.hand(color);
    score
        + hand.count(PieceType::Pawn)
        + hand.count(PieceType::Lance)
        + hand.count(PieceType::Knight)
        + hand.count(PieceType::Silver)
        + hand.count(PieceType::Gold)
        + (hand.count(PieceType::Bishop) + hand.count(PieceType::Rook)) * 5
}

fn piece_point(pt: PieceType) -> u32 {
    match pt.unpromote() {
        PieceType::Bishop | PieceType::Rook => 5,
        PieceType::King => 0,
        _ => 1,
    }
}

fn king_entered(pos: &Position, color: Color) -> bool {
    enemy_field_ranks(color).contains(pos.king_square(color))
}

fn enemy_field_ranks(color: Color) -> rshogi_core::bitboard::Bitboard {
    use rshogi_core::bitboard::RANK_BB;
    match color {
        Color::Black => RANK_BB[0] | RANK_BB[1] | RANK_BB[2],
        Color::White => RANK_BB[6] | RANK_BB[7] | RANK_BB[8],
    }
}

fn parse_rate_comment(line: &str) -> Option<u32> {
    let value = line.rsplit(':').next()?;
    value
        .parse::<u32>()
        .ok()
        .or_else(|| value.parse::<f64>().ok().map(|v| v as u32))
}

fn parse_eval_comment(line: &str) -> Option<i32> {
    let rest = line.strip_prefix("'** ")?;
    rest.split_whitespace().next()?.parse().ok()
}

fn is_csa_move(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() >= 7
        && matches!(bytes[0], b'+' | b'-')
        && bytes[1..5].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_uppercase)
}

fn csa_side(raw: &str) -> Result<Color> {
    match raw.as_bytes().first().copied() {
        Some(b'+') => Ok(Color::Black),
        Some(b'-') => Ok(Color::White),
        _ => bail!("CSA 手番符号が不正です: {raw}"),
    }
}

fn normalize_csa_eval(eval_raw: i32, side: Color) -> i32 {
    if side == Color::Black {
        eval_raw
    } else {
        -eval_raw
    }
}

fn update_sign_validation(
    path: &Path,
    game_id: &str,
    game: &CsaGame,
    last_eval: Option<i32>,
    last_side: Option<Color>,
    stats: &mut Stats,
) {
    if game.end_kind != "%TORYO" {
        return;
    }
    let (Some(raw), Some(side)) = (last_eval, last_side) else {
        return;
    };
    let resign_side = !side;
    let ok = if resign_side == Color::Black {
        raw < 0
    } else {
        raw > 0
    };
    stats.sign_validation.checked_toryo_games += 1;
    if ok {
        stats.sign_validation.resign_side_negative += 1;
    } else {
        stats.sign_validation.contradictory += 1;
    }
    if stats.sign_validation.samples.len() < 20 {
        stats.sign_validation.samples.push(SignSample {
            game_id: game_id.to_string(),
            file: path.display().to_string(),
            resign_side: color_label(resign_side),
            last_eval_raw: raw,
            normalized_black: normalize_csa_eval(raw, side),
            ok,
        });
    }
}

fn result_from_csa(end_kind: &str, last_move_side: Color) -> String {
    match end_kind {
        "%TORYO" | "%TIME_UP" | "%ILLEGAL_MOVE" => {
            if last_move_side == Color::Black {
                "black_win"
            } else {
                "white_win"
            }
        }
        "%KACHI" => {
            if last_move_side == Color::Black {
                "black_win"
            } else {
                "white_win"
            }
        }
        "%SENNICHITE" | "%HIKIWAKE" | "%JISHOGI" => "draw",
        _ => "unknown",
    }
    .to_string()
}

fn color_label(color: Color) -> char {
    if color == Color::Black { 'b' } else { 'w' }
}

fn progress_band(ply: u32) -> &'static str {
    match ply {
        1..=40 => "1-40",
        41..=80 => "41-80",
        81..=120 => "81-120",
        _ => "121+",
    }
}

fn eval_band(eval: Option<i32>) -> &'static str {
    let Some(eval) = eval else {
        return "unknown";
    };
    match eval.abs() {
        0..=150 => "0-150",
        151..=600 => "151-600",
        601..=1500 => "601-1500",
        1501..=29_999 => "1501+",
        _ => "mate",
    }
}

fn update_population(record: &BenchRecord, stats: &mut Stats) {
    stats.total_positions += 1;
    add_count(&mut stats.source_positions, source_key(record.source));
    add_count(&mut stats.population_by_cell, &cell_key(record));
}

fn maybe_set_startpos(candidate: &Candidate, cli: &Cli, slot: &mut Option<Candidate>) {
    let lower = cli.startpos_ply.saturating_sub(cli.startpos_window);
    let upper = cli.startpos_ply.saturating_add(cli.startpos_window);
    let Some(eval) = candidate.record.eval_cp_black else {
        return;
    };
    if slot.is_none()
        && (lower..=upper).contains(&candidate.record.ply)
        && eval.abs() <= cli.startpos_eval_abs_max
    {
        *slot = Some(candidate.clone());
    }
}

fn stratified_sample(
    all: &[Candidate],
    per_cell: usize,
    rng: &mut ChaCha8Rng,
    stats: &mut Stats,
) -> Vec<Candidate> {
    let mut cells: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for candidate in all {
        cells.entry(cell_key(&candidate.record)).or_default().push(candidate.clone());
    }
    let mut out = Vec::new();
    for (cell, mut values) in cells {
        values.shuffle(rng);
        let take = values.len().min(per_cell);
        add_count_by(&mut stats.accepted_by_cell, &cell, take as u64);
        out.extend(values.into_iter().take(take));
    }
    out
}

fn sample_nyugyoku(mut values: Vec<Candidate>, max: usize, rng: &mut ChaCha8Rng) -> Vec<Candidate> {
    values.shuffle(rng);
    values.truncate(max);
    values
}

fn dedup_startpos(values: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for candidate in values {
        if seen.insert(candidate.record.sfen.clone()) {
            out.push(candidate);
        }
    }
    out
}

fn cell_key(record: &BenchRecord) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        record.progress_band, record.eval_band, record.nyugyoku, record.in_check, record.stm
    )
}

fn source_key(source: Source) -> &'static str {
    match source {
        Source::Floodgate => "floodgate",
        Source::Selfplay => "selfplay",
    }
}

fn add_count(map: &mut BTreeMap<String, u64>, key: &str) {
    add_count_by(map, key, 1);
}

fn add_count_by(map: &mut BTreeMap<String, u64>, key: &str, value: u64) {
    *map.entry(key.to_string()).or_default() += value;
}

fn score_from_eval(eval: Option<&JsonlEval>) -> Option<i32> {
    let eval = eval?;
    if let Some(cp) = eval.score_cp {
        Some(cp.clamp(-30_000, 30_000))
    } else {
        eval.score_mate.map(|mate| if mate > 0 { 30_000 } else { -30_000 })
    }
}

fn is_terminal_move(move_usi: &str) -> bool {
    matches!(move_usi, "resign" | "win" | "timeout" | "illegal" | "none")
}

fn usi_line(moves: &[String]) -> String {
    if moves.is_empty() {
        "startpos".to_string()
    } else {
        format!("startpos moves {}", moves.join(" "))
    }
}

fn write_jsonl<'a, I>(path: &Path, records: I) -> Result<()>
where
    I: IntoIterator<Item = &'a BenchRecord>,
{
    let file = File::create(path).with_context(|| format!("出力できません: {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for record in records {
        serde_json::to_writer(&mut writer, record)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

fn write_startpos(path: &Path, records: &[Candidate]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("出力できません: {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for record in records {
        writeln!(writer, "{}", record.usi_line)?;
    }
    writer.flush()?;
    Ok(())
}

fn write_stats(path: &Path, stats: &Stats) -> Result<()> {
    let file = File::create(path).with_context(|| format!("出力できません: {}", path.display()))?;
    serde_json::to_writer_pretty(file, stats)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csa_eval_is_normalized_to_black_view() {
        assert_eq!(normalize_csa_eval(120, Color::Black), 120);
        assert_eq!(normalize_csa_eval(120, Color::White), -120);
    }

    #[test]
    fn csa_move_matches_startpos_legal_move() {
        let mut pos = Position::new();
        pos.set_hirate();
        let mv = csa_to_legal_move(&pos, "+7776FU").expect("legal csa");
        assert_eq!(mv.to_usi(), "7g7f");
    }

    #[test]
    fn jsonl_score_uses_mate_sentinel() {
        assert_eq!(
            score_from_eval(Some(&JsonlEval {
                score_cp: None,
                score_mate: Some(-3)
            })),
            Some(-30_000)
        );
    }

    #[test]
    fn parse_minimal_csa_fixture() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("game.csa");
        fs::write(
            &path,
            concat!(
                "V2.2\n",
                "N+black\n",
                "N-white\n",
                "'black_rate:black:3200\n",
                "'white_rate:white:3100\n",
                "PI\n",
                "+\n",
                "+7776FU\n",
                "'** 30 7776FU\n",
                "-3334FU\n",
                "'** -20 3334FU\n",
                "%TORYO\n",
            ),
        )
        .expect("write csa");
        let game = parse_csa(&path).expect("parse csa");
        assert_eq!(game.moves.len(), 2);
        assert_eq!(game.black_rate, Some(3200));
        assert_eq!(game.white_rate, Some(3100));
        assert_eq!(game.moves[0].eval_raw, Some(30));
    }
}
