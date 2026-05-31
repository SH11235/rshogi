//! tournament/analyze_selfplay 互換 JSONL → PSV 変換ツール
//!
//! `runs/selfplay` 以下にある tournament の対局ログから、各 `move` 行の
//! `sfen_before` と `move_usi` を PackedSfenValue として書き出す。
//! `score` はログの `eval.score_cp` / `eval.score_mate` から暫定値を入れるが、
//! 後段で `rescore_psv` する前提なら `--missing-score zero` で欠損スコアも
//! 0 として残せる。

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use rshogi_core::movegen::is_legal_with_pass;
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move};
use serde::Deserialize;
use serde_json::Value;
use tools::common::dedup::collect_input_paths;
use tools::packed_sfen::{PackedSfenValue, move_to_move16, pack_position};

#[derive(Parser, Debug)]
#[command(
    name = "jsonl_to_psv",
    about = "tournament JSONL を PackedSfenValue 形式へ変換"
)]
struct Args {
    /// 入力 JSONL ファイル、ディレクトリ、glob（カンマ区切り可）。--input-dir と排他
    #[arg(long)]
    input: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせて再帰的に収集する
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir またはディレクトリ入力時の glob パターン
    #[arg(long, default_value = "*.jsonl")]
    pattern: String,

    /// 出力 PSV ファイル
    #[arg(short, long)]
    output: PathBuf,

    /// スコア欠損局面の扱い
    #[arg(long, value_enum, default_value_t = MissingScoreMode::Skip)]
    missing_score: MissingScoreMode,

    /// 変換する最大対局数（0 = 全件）
    #[arg(long, default_value_t = 0)]
    max_games: u64,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum MissingScoreMode {
    /// eval がない局面はスキップする
    Skip,
    /// eval がない局面は score=0 として書き出す
    Zero,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LogEntry {
    Move(MoveLog),
    Result(ResultLog),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct MoveLog {
    game_id: u32,
    ply: u32,
    side_to_move: char,
    sfen_before: String,
    move_usi: String,
    #[serde(default)]
    eval: Option<EvalLog>,
}

#[derive(Debug, Deserialize)]
struct EvalLog {
    score_cp: Option<i32>,
    score_mate: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct ResultLog {
    game_id: u32,
    outcome: Outcome,
    #[serde(default)]
    error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    BlackWin,
    WhiteWin,
    Draw,
    InProgress,
}

#[derive(Debug, Clone, Copy)]
struct PendingRecord {
    packed_sfen: [u8; 32],
    score: i16,
    move16: u16,
    game_ply: u16,
    side_to_move: Color,
}

#[derive(Debug, Default)]
struct Stats {
    files: u64,
    games_seen: u64,
    games_written: u64,
    positions_written: u64,
    skipped_missing_score: u64,
    skipped_terminal_move: u64,
    skipped_error_game: u64,
    skipped_in_progress_game: u64,
    parse_errors: u64,
    move_errors: u64,
    orphan_games: u64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let paths = collect_input_paths(args.input.as_deref(), args.input_dir.as_ref(), &args.pattern)
        .context("入力ファイルの収集に失敗しました")?;
    if paths.is_empty() {
        bail!("入力ファイルが見つかりません");
    }

    let output_canonical = args.output.canonicalize().ok();
    for path in &paths {
        if let Ok(input_canonical) = path.canonicalize()
            && Some(&input_canonical) == output_canonical.as_ref()
        {
            bail!("出力ファイルが入力ファイルと同一です: {}", path.display());
        }
    }

    let out_file = File::create(&args.output)
        .with_context(|| format!("出力ファイルを作成できません: {}", args.output.display()))?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, out_file);

    let mut total = Stats::default();
    let mut games_written = 0u64;

    for path in paths {
        if args.max_games > 0 && games_written >= args.max_games {
            break;
        }
        eprintln!("Reading: {}", path.display());
        let stats = process_file(
            &path,
            &mut writer,
            args.missing_score,
            args.max_games,
            &mut games_written,
        )
        .with_context(|| format!("変換に失敗しました: {}", path.display()))?;
        total.add(stats);
    }

    writer.flush()?;

    println!("=== JSONL → PSV Summary ===");
    println!("Files:                 {}", total.files);
    println!("Games seen:            {}", total.games_seen);
    println!("Games written:         {}", total.games_written);
    println!("Positions written:     {}", total.positions_written);
    println!("Skipped missing score: {}", total.skipped_missing_score);
    println!("Skipped terminal move: {}", total.skipped_terminal_move);
    println!("Skipped error games:   {}", total.skipped_error_game);
    println!("Skipped in-progress:   {}", total.skipped_in_progress_game);
    println!("Move errors:           {}", total.move_errors);
    println!("Parse errors:          {}", total.parse_errors);
    println!("Orphan games:          {}", total.orphan_games);
    println!("Output file:           {}", args.output.display());
    println!(
        "Output size:           {:.1} MB",
        (total.positions_written * PackedSfenValue::SIZE as u64) as f64 / (1024.0 * 1024.0)
    );

    Ok(())
}

impl Stats {
    fn add(&mut self, rhs: Stats) {
        self.files += rhs.files;
        self.games_seen += rhs.games_seen;
        self.games_written += rhs.games_written;
        self.positions_written += rhs.positions_written;
        self.skipped_missing_score += rhs.skipped_missing_score;
        self.skipped_terminal_move += rhs.skipped_terminal_move;
        self.skipped_error_game += rhs.skipped_error_game;
        self.skipped_in_progress_game += rhs.skipped_in_progress_game;
        self.parse_errors += rhs.parse_errors;
        self.move_errors += rhs.move_errors;
        self.orphan_games += rhs.orphan_games;
    }
}

fn process_file(
    path: &Path,
    writer: &mut BufWriter<File>,
    missing_score: MissingScoreMode,
    max_games: u64,
    games_written: &mut u64,
) -> Result<Stats> {
    let file = File::open(path)
        .with_context(|| format!("入力ファイルを開けません: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut pending: HashMap<u32, Vec<PendingRecord>> = HashMap::new();
    let mut stats = Stats {
        files: 1,
        ..Stats::default()
    };

    for (line_idx, line) in reader.lines().enumerate() {
        if max_games > 0 && *games_written >= max_games {
            break;
        }

        let line = line.with_context(|| format!("{}行目の読み込みに失敗", line_idx + 1))?;
        if line.trim().is_empty() {
            continue;
        }

        let entry = match serde_json::from_str::<LogEntry>(&line) {
            Ok(entry) => entry,
            Err(_) => match serde_json::from_str::<Value>(&line) {
                Ok(_) => {
                    stats.parse_errors += 1;
                    continue;
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("{}行目のJSONが不正", line_idx + 1));
                }
            },
        };

        match entry {
            LogEntry::Move(mv) => match convert_move(&mv, missing_score) {
                Ok(Some(record)) => pending.entry(mv.game_id).or_default().push(record),
                Ok(None) => {
                    if is_terminal_move(&mv.move_usi) {
                        stats.skipped_terminal_move += 1;
                    } else {
                        stats.skipped_missing_score += 1;
                    }
                }
                Err(e) => {
                    stats.move_errors += 1;
                    eprintln!(
                        "  move変換エラー {}:{} game_id={} ply={}: {e}",
                        path.display(),
                        line_idx + 1,
                        mv.game_id,
                        mv.ply
                    );
                }
            },
            LogEntry::Result(result) => {
                stats.games_seen += 1;
                let records = pending.remove(&result.game_id).unwrap_or_default();
                if result.error {
                    stats.skipped_error_game += 1;
                    continue;
                }
                if result.outcome == Outcome::InProgress {
                    stats.skipped_in_progress_game += 1;
                    continue;
                }
                if records.is_empty() {
                    continue;
                }
                write_game(writer, &records, result.outcome)?;
                stats.games_written += 1;
                stats.positions_written += records.len() as u64;
                *games_written += 1;
            }
            LogEntry::Other => {}
        }
    }

    stats.orphan_games += pending.len() as u64;
    Ok(stats)
}

fn convert_move(mv: &MoveLog, missing_score: MissingScoreMode) -> Result<Option<PendingRecord>> {
    if is_terminal_move(&mv.move_usi) {
        return Ok(None);
    }

    let score = match mv.eval.as_ref().and_then(score_from_eval) {
        Some(score) => score,
        None if matches!(missing_score, MissingScoreMode::Zero) => 0,
        None => return Ok(None),
    };

    let mut pos = Position::new();
    pos.set_sfen(&mv.sfen_before)
        .with_context(|| format!("SFENパース失敗: {}", mv.sfen_before))?;

    let side_to_move = color_from_label(mv.side_to_move)
        .with_context(|| format!("不明な手番ラベル: {}", mv.side_to_move))?;
    if pos.side_to_move() != side_to_move {
        bail!(
            "SFENの手番({})とログの手番({})が一致しません",
            color_label(pos.side_to_move()),
            mv.side_to_move
        );
    }

    let best_move = Move::from_usi(&mv.move_usi)
        .with_context(|| format!("USI指し手パース失敗: {}", mv.move_usi))?;
    if best_move == Move::PASS {
        bail!("PASS手はPackedSfenValueにエンコードできません");
    }
    if !is_legal_with_pass(&pos, best_move) {
        bail!("非合法手です: {}", mv.move_usi);
    }

    Ok(Some(PendingRecord {
        packed_sfen: pack_position(&pos),
        score,
        move16: move_to_move16(best_move),
        game_ply: pos.game_ply().clamp(0, u16::MAX as i32) as u16,
        side_to_move,
    }))
}

fn write_game(
    writer: &mut BufWriter<File>,
    records: &[PendingRecord],
    outcome: Outcome,
) -> io::Result<()> {
    for record in records {
        let psv = PackedSfenValue {
            sfen: record.packed_sfen,
            score: record.score,
            move16: record.move16,
            game_ply: record.game_ply,
            game_result: game_result_for_side(outcome, record.side_to_move),
            padding: 0,
        };
        writer.write_all(&psv.to_bytes())?;
    }
    Ok(())
}

fn score_from_eval(eval: &EvalLog) -> Option<i16> {
    if let Some(cp) = eval.score_cp {
        return Some(cp.clamp(-10000, 10000) as i16);
    }
    eval.score_mate.map(|mate| {
        if mate > 0 {
            10000
        } else if mate < 0 {
            -10000
        } else {
            0
        }
    })
}

fn game_result_for_side(outcome: Outcome, side: Color) -> i8 {
    match outcome {
        Outcome::BlackWin => {
            if side == Color::Black {
                1
            } else {
                -1
            }
        }
        Outcome::WhiteWin => {
            if side == Color::White {
                1
            } else {
                -1
            }
        }
        Outcome::Draw | Outcome::InProgress => 0,
    }
}

fn color_from_label(label: char) -> Option<Color> {
    match label {
        'b' => Some(Color::Black),
        'w' => Some(Color::White),
        _ => None,
    }
}

fn color_label(color: Color) -> char {
    if color == Color::Black { 'b' } else { 'w' }
}

fn is_terminal_move(move_usi: &str) -> bool {
    matches!(move_usi, "resign" | "win" | "timeout" | "illegal" | "none")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn score_from_eval_prefers_cp_and_clamps() {
        assert_eq!(
            score_from_eval(&EvalLog {
                score_cp: Some(12000),
                score_mate: Some(-3),
            }),
            Some(10000)
        );
        assert_eq!(
            score_from_eval(&EvalLog {
                score_cp: None,
                score_mate: Some(-7),
            }),
            Some(-10000)
        );
    }

    #[test]
    fn result_is_from_side_to_move_viewpoint() {
        assert_eq!(game_result_for_side(Outcome::BlackWin, Color::Black), 1);
        assert_eq!(game_result_for_side(Outcome::BlackWin, Color::White), -1);
        assert_eq!(game_result_for_side(Outcome::WhiteWin, Color::Black), -1);
        assert_eq!(game_result_for_side(Outcome::Draw, Color::White), 0);
    }

    #[test]
    fn converts_minimal_tournament_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("game.jsonl");
        let output = dir.path().join("out.psv");
        std::fs::write(
            &input,
            concat!(
                "{\"type\":\"move\",\"game_id\":1,\"ply\":1,\"side_to_move\":\"b\",",
                "\"sfen_before\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\",",
                "\"move_usi\":\"7g7f\",\"engine\":\"black\",\"elapsed_ms\":1,\"think_limit_ms\":1,",
                "\"timed_out\":false,\"eval\":{\"score_cp\":23}}\n",
                "{\"type\":\"move\",\"game_id\":1,\"ply\":2,\"side_to_move\":\"w\",",
                "\"sfen_before\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2\",",
                "\"move_usi\":\"3c3d\",\"engine\":\"white\",\"elapsed_ms\":1,\"think_limit_ms\":1,",
                "\"timed_out\":false,\"eval\":{\"score_mate\":-5}}\n",
                "{\"type\":\"result\",\"game_id\":1,\"outcome\":\"black_win\",\"reason\":\"resign\",\"plies\":2}\n",
            ),
        )
        .expect("write input");

        let mut writer = BufWriter::new(File::create(&output).expect("create output"));
        let mut games_written = 0;
        let stats =
            process_file(&input, &mut writer, MissingScoreMode::Skip, 0, &mut games_written)
                .expect("process file");
        writer.flush().expect("flush");

        assert_eq!(stats.games_written, 1);
        assert_eq!(stats.positions_written, 2);
        assert_eq!(games_written, 1);

        let mut bytes = Vec::new();
        File::open(&output)
            .expect("open output")
            .read_to_end(&mut bytes)
            .expect("read output");
        assert_eq!(bytes.len(), PackedSfenValue::SIZE * 2);

        let first = PackedSfenValue::from_bytes(&bytes[..PackedSfenValue::SIZE]).expect("first");
        let second = PackedSfenValue::from_bytes(&bytes[PackedSfenValue::SIZE..]).expect("second");
        assert_eq!(first.score, 23);
        assert_eq!(first.game_ply, 1);
        assert_eq!(first.game_result, 1);
        assert_eq!(second.score, -10000);
        assert_eq!(second.game_ply, 2);
        assert_eq!(second.game_result, -1);
    }
}
