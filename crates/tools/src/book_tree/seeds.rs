//! 自己対局の定跡出口から、次に読む木の根と必読手順を集める。

use super::audit::reject_same_file;
use super::refutation::format_queue;
use super::targets::{Line, Opts, Root, parse_root};
use super::walk::{book_probe, check_tag, excluded_positions, open_book, queue_id};
use crate::common::io::write_atomic;
use crate::selfplay::types::PrimaryScore;
use crate::tournament_pairs::ResultPairs;
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub(super) struct SeedsArgs {
    #[arg(long, required = true)]
    games: Vec<PathBuf>,
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    table: PathBuf,
    #[arg(long)]
    tag: String,
    #[arg(long, default_value_t = 6)]
    depth: usize,
    #[arg(long)]
    opp_n: Option<usize>,
    #[arg(long)]
    stop: Option<i32>,
    #[arg(long)]
    rounds: Option<usize>,
    #[arg(long, allow_hyphen_values = true)]
    draw_black: Option<i32>,
    #[arg(long, allow_hyphen_values = true)]
    draw_white: Option<i32>,
    #[arg(long, value_delimiter = ',', default_value = "0,6,12")]
    offsets: Vec<usize>,
    #[arg(long, default_value_t = 2)]
    min_games_offset: usize,
    #[arg(long, default_value_t = 3)]
    max_lines: usize,
    #[arg(long, default_value_t = 30)]
    collapse_plies: u32,
    #[arg(long, default_value_t = 150)]
    collapse_cp: i32,
    #[arg(long, default_value_t = 2)]
    priority_early: u32,
    #[arg(long, default_value_t = 100)]
    priority_late: u32,
    #[arg(long)]
    opp_freq: Option<PathBuf>,
    #[arg(long)]
    opp_book: Option<PathBuf>,
    #[arg(long)]
    exclude_queue: Vec<PathBuf>,
}

impl SeedsArgs {
    fn opts(&self) -> Opts {
        Opts {
            opp_n: self.opp_n,
            stop: self.stop,
            rounds: self.rounds,
            draw_black: self.draw_black,
            draw_white: self.draw_white,
            ..Opts::default()
        }
    }
}

#[derive(Clone, Deserialize)]
struct GameResult {
    game_id: u32,
    outcome: String,
    #[serde(default)]
    pair_index: Option<u32>,
    #[serde(default)]
    pair_slot: Option<u32>,
    #[serde(default)]
    attempt: u32,
    #[serde(default)]
    error: Option<bool>,
    #[serde(default)]
    plies: Option<u32>,
}

#[derive(Deserialize)]
struct Move {
    game_id: u32,
    ply: u32,
    side_to_move: String,
    sfen_before: String,
    move_usi: String,
    eval: Option<Eval>,
    #[serde(default)]
    timed_out: bool,
}

#[derive(Deserialize)]
struct Eval {
    last_exact_primary: Option<Primary>,
}

#[derive(Deserialize)]
struct Primary {
    score: PrimaryScore,
}

fn visit_json(path: &Path, mut visit: impl FnMut(serde_json::Value) -> Result<()>) -> Result<()> {
    for (n, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let context = || format!("{}:{}", path.display(), n + 1);
        let value = serde_json::from_str(&line).with_context(context)?;
        visit(value).with_context(context)?;
    }
    Ok(())
}

fn valid_results(path: &Path) -> Result<BTreeMap<u32, GameResult>> {
    let mut pairs = ResultPairs::default();
    let mut valid = BTreeMap::new();
    let mut seq = 0;
    let mut in_progress = 0;
    visit_json(path, |value| {
        if value["type"] != "result" {
            return Ok(());
        }
        let result: GameResult = serde_json::from_value(value)?;
        let key = (result.pair_index.unwrap_or(seq / 2), result.attempt);
        let slot = result.pair_slot.unwrap_or(seq % 2).min(1) as usize;
        seq += 1;
        if let Some(pair) = pairs.push(path, key, slot, result)
            && pair.iter().all(|r| !r.error.unwrap_or(false))
        {
            for result in pair {
                if result.outcome == "in_progress" {
                    in_progress += 1;
                    continue;
                }
                ensure!(
                    matches!(result.outcome.as_str(), "black_win" | "white_win" | "draw"),
                    "不正な outcome: {}",
                    result.outcome
                );
                ensure!(
                    !valid.contains_key(&result.game_id),
                    "game_id が重複しています: {}",
                    result.game_id
                );
                valid.insert(result.game_id, result);
            }
        }
        Ok(())
    })?;
    if in_progress > 0 {
        println!("警告: {}: outcome=in_progress の {in_progress} 局を種から除外", path.display());
    }
    if pairs.incomplete() > 0 {
        eprintln!("{}: 未完了の {} ペアを除外", path.display(), pairs.incomplete());
    }
    Ok(valid)
}

// tournament は複数局の move を交互に書くため、終局していない局の手順だけを保持する。
fn visit_games(
    path: &Path,
    valid: &BTreeMap<u32, GameResult>,
    mut visit: impl FnMut(&GameResult, &[Move]) -> Result<()>,
) -> Result<()> {
    let mut active: BTreeMap<u32, Vec<Move>> = BTreeMap::new();
    let mut done = BTreeSet::new();
    visit_json(path, |value| {
        match value["type"].as_str() {
            Some("move") => {
                let id: u32 = serde_json::from_value(value["game_id"].clone())?;
                if valid.contains_key(&id) && !done.contains(&id) {
                    let mv: Move = serde_json::from_value(value)?;
                    active.entry(mv.game_id).or_default().push(mv);
                }
            }
            Some("result") => {
                let id: u32 = serde_json::from_value(value["game_id"].clone())?;
                if let Some(result) = valid.get(&id)
                    && done.insert(id)
                {
                    let moves = active.remove(&id).unwrap_or_default();
                    for pair in moves.windows(2) {
                        ensure!(
                            pair[0].ply.checked_add(1) == Some(pair[1].ply),
                            "手順が不連続: game_id={id}"
                        );
                    }
                    visit(result, &moves)?;
                }
            }
            _ => {}
        }
        Ok(())
    })
}

fn played(mv: &Move) -> bool {
    !mv.timed_out
        && rshogi_core::types::Move::from_usi(&mv.move_usi)
            .is_some_and(|mv| mv.is_normal() || mv.is_pass())
}

fn root_at(moves: &[Move], index: usize) -> Result<Option<Root>> {
    if let Some(mv) = moves.get(index) {
        return parse_root(&format!("sfen {}", mv.sfen_before)).map(Some);
    }
    if index == moves.len()
        && let Some(last) = moves.last().filter(|m| played(m))
    {
        let last_root = parse_root(&format!("sfen {}", last.sfen_before))?;
        let key = super::model::child(&last_root.key, &last.move_usi)?;
        let ply = last_root.ply.checked_add(1).context("根の手数が overflow")?;
        return Ok(Some(Root { key, ply }));
    }
    Ok(None)
}

fn origin(moves: &[Move]) -> Option<usize> {
    moves.iter().position(|m| m.eval.is_some())
}

fn early(
    result: &GameResult,
    moves: &[Move],
    start: usize,
    own: &str,
    args: &SeedsArgs,
) -> Result<bool> {
    let first = moves[start].ply;
    let loses = matches!((own, result.outcome.as_str()), ("b", "white_win") | ("w", "black_win"));
    let end = result.plies.unwrap_or_else(|| moves.last().map_or(first, |m| m.ply));
    if loses && end.saturating_sub(first.saturating_sub(1)) <= args.collapse_plies {
        return Ok(true);
    }
    for mv in &moves[start..] {
        if mv.ply - first > args.collapse_plies {
            break;
        }
        let Some(primary) = mv.eval.as_ref().and_then(|e| e.last_exact_primary.as_ref()) else {
            continue;
        };
        ensure!(matches!(mv.side_to_move.as_str(), "b" | "w"), "不正な side_to_move");
        let score = match primary.score {
            PrimaryScore::Cp(cp) => i64::from(cp),
            PrimaryScore::Mate(n) => {
                if n > 0 {
                    i64::MAX
                } else {
                    -i64::MAX
                }
            }
            PrimaryScore::MateWin => i64::MAX,
            PrimaryScore::MateLoss => -i64::MAX,
        };
        let own_score = if mv.side_to_move == own {
            score
        } else {
            -score
        };
        if own_score <= -i64::from(args.collapse_cp) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Default)]
struct RootData {
    ply: u32,
    games: BTreeSet<String>,
    lines: BTreeMap<Vec<String>, BTreeSet<String>>,
    early: bool,
}

impl RootData {
    fn merge(&mut self, other: Self) {
        if self.ply == 0 || other.ply < self.ply {
            self.ply = other.ply;
        }
        self.games.extend(other.games);
        self.early |= other.early;
        for (line, ids) in other.lines {
            self.lines.entry(line).or_default().extend(ids);
        }
    }
}

pub(super) fn run(mut args: SeedsArgs) -> Result<()> {
    check_tag(&args.tag)?;
    args.opts().check()?;
    ensure!(args.depth <= super::targets::MAX_DEPTH, "depth が大きすぎます");
    ensure!(
        args.min_games_offset > 0 && args.max_lines > 0 && args.collapse_cp > 0,
        "min-games-offset、max-lines、collapse-cp は正数が必要です"
    );
    ensure!(
        args.priority_early <= super::targets::MAX_PRIORITY
            && args.priority_late <= super::targets::MAX_PRIORITY,
        "priority が大きすぎます"
    );
    args.games = args
        .games
        .iter()
        .map(|p| p.canonicalize().with_context(|| format!("--games: {}", p.display())))
        .collect::<Result<_>>()?;
    args.games.sort();
    args.games.dedup();
    args.offsets.push(0);
    args.offsets.sort_unstable();
    args.offsets.dedup();
    let mut inputs: Vec<_> = args.games.iter().map(|p| ("--games", p.as_path())).collect();
    inputs.extend(args.exclude_queue.iter().map(|p| ("--exclude-queue", p.as_path())));
    if let Some(p) = &args.opp_freq {
        inputs.push(("--opp-freq", p));
    }
    if let Some(p) = &args.opp_book {
        inputs.push(("--opp-book", p));
    }
    reject_same_file(&args.table, &inputs)?;
    inputs.push(("--table", &args.table));
    reject_same_file(&args.out, &inputs)?;
    let valid: Vec<_> = args.games.iter().map(|p| valid_results(p)).collect::<Result<_>>()?;
    let mut non_sweep = BTreeSet::new();
    let mut non_sweep_roots = BTreeSet::new();
    for (path, valid) in args.games.iter().zip(&valid) {
        visit_games(path, valid, |result, moves| {
            if result.outcome != "black_win"
                && let Some(start) = origin(moves)
            {
                non_sweep.insert(parse_root(&format!("sfen {}", moves[start].sfen_before))?.key);
                for &offset in &args.offsets {
                    if let Some(index) = start.checked_add(offset)
                        && let Some(root) = root_at(moves, index)?
                    {
                        non_sweep_roots.insert(root.key);
                    }
                }
            }
            Ok(())
        })?;
    }
    // オフセット先は、合流前の起点と完全に同じ接頭手順で局数を判定する。
    let mut prefixes: BTreeMap<(String, String, Vec<String>), (String, RootData)> = BTreeMap::new();
    for (file, (path, valid)) in args.games.iter().zip(&valid).enumerate() {
        visit_games(path, valid, |result, moves| {
            let Some(start) = origin(moves) else {
                return Ok(());
            };
            let origin_key = parse_root(&format!("sfen {}", moves[start].sfen_before))?.key;
            if !non_sweep.contains(&origin_key) {
                return Ok(());
            }
            for own in ["b", "w"] {
                if own == "w" && result.outcome == "black_win" {
                    continue;
                }
                let early = early(result, moves, start, own, &args)?;
                for &offset in &args.offsets {
                    let Some(index) = start.checked_add(offset) else {
                        continue;
                    };
                    let Some(root) = root_at(moves, index)? else {
                        continue;
                    };
                    let prefix: Vec<_> =
                        moves[start..index].iter().map(|m| m.move_usi.clone()).collect();
                    let line: Vec<_> = moves[index..]
                        .iter()
                        .take(args.depth + 1)
                        .take_while(|m| played(m))
                        .map(|m| m.move_usi.clone())
                        .collect();
                    let id = format!("{file}:{}", result.game_id);
                    let mut data = RootData {
                        ply: root.ply,
                        early,
                        ..RootData::default()
                    };
                    data.games.insert(id.clone());
                    if !line.is_empty() {
                        data.lines.entry(line).or_default().insert(id);
                    }
                    let entry = prefixes
                        .entry((origin_key.clone(), own.to_owned(), prefix))
                        .or_insert_with(|| (root.key, RootData::default()));
                    entry.1.merge(data);
                }
            }
            Ok(())
        })?;
    }
    let mut roots: BTreeMap<(String, String), RootData> = BTreeMap::new();
    for ((_, own, prefix), (key, data)) in prefixes {
        if !prefix.is_empty() && data.games.len() < args.min_games_offset {
            continue;
        }
        roots.entry((key, own)).or_default().merge(data);
    }
    let excluded = excluded_positions(&args.exclude_queue)?;
    let freq = args.opp_freq.as_deref().map(open_book).transpose()?;
    let book = args.opp_book.as_deref().map(open_book).transpose()?;
    let mut freq = freq.as_ref().map(book_probe);
    let mut book = book.as_ref().map(book_probe);
    let mut excluded_counts = BTreeMap::<String, usize>::new();
    let mut picked = Vec::new();
    for ((key, own), data) in roots {
        if own == "b" && !non_sweep_roots.contains(&key) {
            continue;
        }
        if excluded.contains_key(&(key.clone(), own.clone())) {
            *excluded_counts.entry(own).or_default() += 1;
            continue;
        }
        let games = freq.as_mut().map(|f| f.games(&key)).transpose()?.unwrap_or(0);
        let in_book = book
            .as_mut()
            .map(|b| b.moves(&key).map(|m| !m.is_empty()))
            .transpose()?
            .unwrap_or(false);
        let weight = u128::from(games) + u128::from(in_book);
        picked.push((key, own, data, weight));
    }
    picked.sort_by(|a, b| {
        b.2.early
            .cmp(&a.2.early)
            .then(b.3.cmp(&a.3))
            .then(a.1.cmp(&b.1))
            .then(a.0.cmp(&b.0))
    });
    let weighted = args.opp_freq.is_some() || args.opp_book.is_some();
    let mut rank = 0u32;
    let mut queue = Vec::new();
    let mut table = String::from("own\troot_ply\tpriority\tclass\tgames\tkey\tgame_ids\tweight\n");
    let mut counts = BTreeMap::<(String, &str), usize>::new();
    for (key, own, data, weight) in picked {
        let class = if data.early { "early" } else { "late" };
        let priority = if data.early {
            args.priority_early
        } else {
            let p = args
                .priority_late
                .checked_add(if weighted { rank } else { 0 })
                .context("priority overflow")?;
            rank = rank.checked_add(1).context("rank overflow")?;
            p
        };
        let mut lines: Vec<_> = data.lines.into_iter().collect();
        lines.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        let line = Line {
            id: queue_id(&args.tag, data.ply as usize, &key, &own),
            root: format!("sfen {key} {}", data.ply),
            roots: Vec::new(),
            own: own.clone(),
            depth: args.depth,
            priority,
            hold: false,
            game: lines.into_iter().take(args.max_lines).map(|(line, _)| line).collect(),
            opts: args.opts(),
            memo: None,
        };
        line.check()?;
        writeln!(
            table,
            "{own}\t{}\t{priority}\t{class}\t{}\t{key}\t{}\t{weight}",
            data.ply,
            data.games.len(),
            data.games.into_iter().collect::<Vec<_>>().join(",")
        )?;
        *counts.entry((own, class)).or_default() += 1;
        queue.push(line);
    }
    let queue_text = format_queue(&queue)?;
    write_atomic(&args.table, &table)?;
    write_atomic(&args.out, &queue_text)?;
    for own in ["b", "w"] {
        println!(
            "{own}: 除外 {} 行 / early {} 行 / late {} 行",
            excluded_counts.get(own).unwrap_or(&0),
            counts.get(&(own.to_owned(), "early")).unwrap_or(&0),
            counts.get(&(own.to_owned(), "late")).unwrap_or(&0)
        );
    }
    println!("キューの行 {}", queue.len());
    Ok(())
}

#[cfg(test)]
mod tests;
