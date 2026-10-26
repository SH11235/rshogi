//! tournament/analyze_selfplay 互換 JSONL → PSV 変換ツール
//!
//! `runs/selfplay` 以下にある tournament の対局ログから、各 `move` 行の
//! `sfen_before` と `move_usi` を PackedSfenValue として書き出す。
//! `score` はログの `eval.score_cp` / `eval.score_mate` から暫定値を入れるが、
//! 後段で `rescore_psv` する前提なら `--missing-score zero` で欠損スコアも
//! 0 として残せる。

use rshogi_csa_client::jsonl::{is_csa_jsonl, is_incomplete_result};
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
use tools::packed_sfen::{PackedSfenValue, move_to_psv_move16, pack_position};

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
    Meta(serde_json::Value),
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
    #[serde(default)]
    reason: Option<String>,
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
    #[serde(alias = "interrupted", alias = "censored")]
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
    truncated_tail_lines: u64,
    corrupt_lines: u64,
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

    let inputs: Vec<_> = paths.iter().map(PathBuf::as_path).collect();
    let out_file = tools::output_path::create_safe_output_file(&args.output, &inputs)?;
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
    println!("Truncated tail lines:  {}", total.truncated_tail_lines);
    println!("Corrupt lines:         {}", total.corrupt_lines);
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
        self.truncated_tail_lines += rhs.truncated_tail_lines;
        self.corrupt_lines += rhs.corrupt_lines;
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
    let mut reader = BufReader::new(file);
    let mut csa = false;
    let mut pending: HashMap<u32, Vec<PendingRecord>> = HashMap::new();
    let mut stats = Stats {
        files: 1,
        ..Stats::default()
    };

    let mut raw = Vec::new();
    let mut line_idx = 0usize;
    loop {
        if max_games > 0 && *games_written >= max_games {
            break;
        }

        raw.clear();
        let read = reader
            .read_until(b'\n', &mut raw)
            .with_context(|| format!("{}行目の読み込みに失敗", line_idx + 1))?;
        if read == 0 {
            break;
        }
        line_idx += 1;
        // 書き手のプロセスが落ちると、最終行が改行なしで途中まで書かれた状態で残る。
        let complete = raw.ends_with(b"\n");

        let line = match std::str::from_utf8(&raw) {
            Ok(line) => line,
            Err(_) => {
                stats.corrupt_lines += 1;
                warn_corrupt_line(path, line_idx, "不正なUTF-8", stats.corrupt_lines);
                continue;
            }
        };
        // 行区切りのみ落とす。JSON 文法上不正な空白 (NBSP 等) を削ると破損行が
        // 健全な行として通ってしまうため trim_end() は使わない。
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        // 空行のみ黙って読み飛ばす。空白だけが残った行は破損の疑いがあるので
        // パースに回し、失敗すれば Corrupt lines に計上する。
        if line.is_empty() {
            continue;
        }

        let entry = match serde_json::from_str::<LogEntry>(line) {
            Ok(entry) => entry,
            Err(_) => match serde_json::from_str::<Value>(line) {
                Ok(_) => {
                    stats.parse_errors += 1;
                    continue;
                }
                Err(_) => {
                    // 書き込み途中で切れた行は record の先頭 `{` だけが残る。
                    // 全損して NUL 等で埋まった行はこれに当たらず破損行として数える。
                    if !complete && line.trim_start().starts_with('{') {
                        stats.truncated_tail_lines += 1;
                        eprintln!(
                            "  警告: {} の最終行({}行目)が書き込み途中のため破棄します",
                            path.display(),
                            line_idx
                        );
                        break;
                    }
                    stats.corrupt_lines += 1;
                    warn_corrupt_line(
                        path,
                        line_idx,
                        "JSONとして解釈できない",
                        stats.corrupt_lines,
                    );
                    continue;
                }
            },
        };

        match entry {
            LogEntry::Meta(meta) => {
                csa = is_csa_jsonl(meta["source"].as_str(), meta["engine_cmd"]["note"].as_str());
            }
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
                        line_idx,
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
                if result.outcome == Outcome::InProgress
                    || (result.outcome == Outcome::Draw
                        && is_incomplete_result(csa, "draw", result.reason.as_deref()))
                {
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
        move16: move_to_psv_move16(best_move),
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

/// 破損行の警告。1 ファイルにつき先頭 5 件までに絞る（全損ファイルで数百万行の
/// 警告を出さないため）。総数は Summary の `Corrupt lines` に出る。
fn warn_corrupt_line(path: &Path, line_idx: usize, reason: &str, corrupt_so_far: u64) {
    const MAX_WARNINGS: u64 = 5;
    if corrupt_so_far <= MAX_WARNINGS {
        eprintln!("  警告: {} の{}行目が{}ため破棄します", path.display(), line_idx, reason);
        if corrupt_so_far == MAX_WARNINGS {
            eprintln!("  警告: {} の破損行の警告は以降省略します", path.display());
        }
    }
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
    fn drops_truncated_last_line_without_failing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("game.jsonl");
        let output = dir.path().join("out.psv");
        std::fs::write(
            &input,
            concat!(
                "{\"type\":\"move\",\"game_id\":1,\"ply\":1,\"side_to_move\":\"b\",",
                "\"sfen_before\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\",",
                "\"move_usi\":\"7g7f\",\"eval\":{\"score_cp\":23}}\n",
                "{\"type\":\"result\",\"game_id\":1,\"outcome\":\"black_win\"}\n",
                // 書き手が落ちて改行なしで切れた行
                "{\"type\":\"move\",\"game_id\":2,\"ply\":1,\"side_to_move\":\"b\",\"sfen_bef",
            ),
        )
        .expect("write input");

        let mut writer = BufWriter::new(File::create(&output).expect("create output"));
        let mut games_written = 0;
        let stats =
            process_file(&input, &mut writer, MissingScoreMode::Skip, 0, &mut games_written)
                .expect("truncated tail must not fail the conversion");
        writer.flush().expect("flush");

        assert_eq!(stats.truncated_tail_lines, 1);
        assert_eq!(stats.games_written, 1);
        assert_eq!(stats.positions_written, 1);
    }

    /// 改行で終わっていない最終行でも、JSON として完結していれば取りこぼさない。
    #[test]
    fn keeps_complete_last_line_without_trailing_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("game.jsonl");
        let output = dir.path().join("out.psv");
        std::fs::write(
            &input,
            concat!(
                "{\"type\":\"move\",\"game_id\":1,\"ply\":1,\"side_to_move\":\"b\",",
                "\"sfen_before\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\",",
                "\"move_usi\":\"7g7f\",\"eval\":{\"score_cp\":23}}\n",
                "{\"type\":\"result\",\"game_id\":1,\"outcome\":\"black_win\"}",
            ),
        )
        .expect("write input");

        let mut writer = BufWriter::new(File::create(&output).expect("create output"));
        let mut games_written = 0;
        let stats =
            process_file(&input, &mut writer, MissingScoreMode::Skip, 0, &mut games_written)
                .expect("process file");
        writer.flush().expect("flush");

        assert_eq!(stats.truncated_tail_lines, 0);
        assert_eq!(stats.corrupt_lines, 0);
        assert_eq!(stats.games_written, 1);
        assert_eq!(stats.positions_written, 1);
    }

    /// 空白だけが残った行は「空行」として黙殺せず破損行に計上する。
    #[test]
    fn counts_whitespace_only_line_as_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("game.jsonl");
        let output = dir.path().join("out.psv");
        std::fs::write(
            &input,
            concat!(
                "\n",         // 純粋な空行は黙って読み飛ばす
                "\u{00a0}\n", // NBSP のみ
                " \t\n",      // 半角空白とタブのみ
                "{\"type\":\"result\",\"game_id\":1,\"outcome\":\"black_win\"}\n",
            ),
        )
        .expect("write input");

        let mut writer = BufWriter::new(File::create(&output).expect("create output"));
        let mut games_written = 0;
        let stats =
            process_file(&input, &mut writer, MissingScoreMode::Skip, 0, &mut games_written)
                .expect("process file");
        writer.flush().expect("flush");

        assert_eq!(stats.corrupt_lines, 2);
        assert_eq!(stats.truncated_tail_lines, 0);
    }

    /// 全損した行が改行なしでファイル末尾にあっても「書き込み途中」ではなく破損行。
    #[test]
    fn counts_garbage_tail_as_corrupt_not_truncated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("game.jsonl");
        let output = dir.path().join("out.psv");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            concat!(
                "{\"type\":\"move\",\"game_id\":1,\"ply\":1,\"side_to_move\":\"b\",",
                "\"sfen_before\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\",",
                "\"move_usi\":\"7g7f\",\"eval\":{\"score_cp\":23}}\n",
                "{\"type\":\"result\",\"game_id\":1,\"outcome\":\"black_win\"}\n",
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&[0u8; 16]);
        std::fs::write(&input, &bytes).expect("write input");

        let mut writer = BufWriter::new(File::create(&output).expect("create output"));
        let mut games_written = 0;
        let stats =
            process_file(&input, &mut writer, MissingScoreMode::Skip, 0, &mut games_written)
                .expect("process file");
        writer.flush().expect("flush");

        assert_eq!(stats.truncated_tail_lines, 0);
        assert_eq!(stats.corrupt_lines, 1);
        assert_eq!(stats.games_written, 1);
    }

    /// 書き手のクラッシュで行の一部が NUL 埋め / 不正 UTF-8 になったログでも、
    /// 健全な行から教師局面を取り出せること。
    #[test]
    fn skips_corrupt_lines_and_keeps_healthy_games() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("game.jsonl");
        let output = dir.path().join("out.psv");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x00\x00\x00\x00\x00\x00\n");
        bytes.extend_from_slice(&[0xff, 0xfe, 0xff, b'\n']);
        bytes.extend_from_slice(
            concat!(
                "{\"type\":\"move\",\"game_id\":1,\"ply\":1,\"side_to_move\":\"b\",",
                "\"sfen_before\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\",",
                "\"move_usi\":\"7g7f\",\"eval\":{\"score_cp\":23}}\n",
                "{\"type\":\"result\",\"game_id\":1,\"outcome\":\"black_win\"}\n",
            )
            .as_bytes(),
        );
        std::fs::write(&input, &bytes).expect("write input");

        let mut writer = BufWriter::new(File::create(&output).expect("create output"));
        let mut games_written = 0;
        let stats =
            process_file(&input, &mut writer, MissingScoreMode::Skip, 0, &mut games_written)
                .expect("corrupt lines must not fail the conversion");
        writer.flush().expect("flush");

        assert_eq!(stats.corrupt_lines, 2);
        assert_eq!(stats.games_written, 1);
        assert_eq!(stats.positions_written, 1);
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
