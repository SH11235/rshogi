//! 自己対局の定跡出口から、次に読む木の根と必読手順を集める。

use super::audit::reject_same_file;
use super::refutation::format_queue;
use super::targets::{Line, Opts, Root, parse_root, read_checked_lines};
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
use std::io::{BufRead, BufReader, Seek, SeekFrom};
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
    #[arg(long, requires = "exclude_queue")]
    update_out: Option<PathBuf>,
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

fn visit_json(
    path: &Path,
    mut visit: impl FnMut(u64, serde_json::Value) -> Result<()>,
) -> Result<()> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = String::new();
    let mut offset = 0;
    let mut n = 0;
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        let start = offset;
        offset += bytes as u64;
        n += 1;
        if line.trim().is_empty() {
            continue;
        }
        let context = || format!("{}:{n}", path.display());
        let value = serde_json::from_str(&line).with_context(context)?;
        visit(start, value).with_context(context)?;
    }
    Ok(())
}

fn valid_game_ids(path: &Path) -> Result<BTreeMap<u32, u64>> {
    let mut pairs = ResultPairs::default();
    let mut valid = BTreeMap::new();
    let mut seq = 0;
    let mut in_progress = 0;
    visit_json(path, |offset, value| {
        if value["type"] != "result" {
            return Ok(());
        }
        let result: GameResult = serde_json::from_value(value)?;
        let key = (result.pair_index.unwrap_or(seq / 2), result.attempt);
        let slot = result.pair_slot.unwrap_or(seq % 2).min(1) as usize;
        seq += 1;
        if let Some(pair) = pairs.push(path, key, slot, (result, offset))
            && pair.iter().all(|(r, _)| !r.error.unwrap_or(false))
        {
            for (result, offset) in pair {
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
                valid.insert(result.game_id, offset);
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

// 有効なスロットは ID と結果行の位置だけ残す。同じ ID の除外結果が先行しても、
// 最初の手順と採用された結果を組み合わせるため、結果本体は必要時に読み直す。
fn visit_games(
    path: &Path,
    valid: &BTreeMap<u32, u64>,
    mut visit: impl FnMut(&GameResult, &[Move]) -> Result<()>,
) -> Result<()> {
    let mut active: BTreeMap<u32, Vec<Move>> = BTreeMap::new();
    let mut remaining = valid.clone();
    let mut result_reader = BufReader::new(File::open(path)?);
    visit_json(path, |offset, value| {
        match value["type"].as_str() {
            Some("move") => {
                let id: u32 = serde_json::from_value(value["game_id"].clone())?;
                if remaining.contains_key(&id) {
                    let mv: Move = serde_json::from_value(value)?;
                    active.entry(mv.game_id).or_default().push(mv);
                }
            }
            Some("result") => {
                let id: u32 = serde_json::from_value(value["game_id"].clone())?;
                if let Some(selected_offset) = remaining.remove(&id) {
                    let result: GameResult = if selected_offset == offset {
                        serde_json::from_value(value)?
                    } else {
                        result_reader.seek(SeekFrom::Start(selected_offset))?;
                        let mut line = String::new();
                        result_reader.read_line(&mut line)?;
                        serde_json::from_str(&line)?
                    };
                    let moves = active.remove(&id).unwrap_or_default();
                    for pair in moves.windows(2) {
                        ensure!(
                            pair[0].ply.checked_add(1) == Some(pair[1].ply),
                            "手順が不連続: game_id={id}"
                        );
                    }
                    visit(&result, &moves)?;
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

fn early(result: &GameResult, moves: &[Move], start: usize, args: &SeedsArgs) -> Result<bool> {
    let first = moves[start].ply;
    let loser = match result.outcome.as_str() {
        "white_win" => Some("b"),
        "black_win" => Some("w"),
        "draw" => None,
        _ => anyhow::bail!("不正な outcome: {}", result.outcome),
    };
    let end = result.plies.unwrap_or_else(|| moves.last().map_or(first, |m| m.ply));
    if end.saturating_sub(first.saturating_sub(1)) <= args.collapse_plies {
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
        let loser_score = match loser {
            Some(side) if mv.side_to_move == side => score,
            Some(_) => -score,
            None => -score.abs(),
        };
        if loser_score <= -i64::from(args.collapse_cp) {
            return Ok(true);
        }
    }
    Ok(false)
}

const ID_SAMPLES: usize = 8;

#[derive(Default)]
struct RootData {
    ply: u32,
    games: u64,
    samples: BTreeSet<String>,
    lines: BTreeMap<(usize, Vec<String>), u64>,
    early: bool,
}

impl RootData {
    fn merge(&mut self, other: Self) {
        if self.ply == 0 || other.ply < self.ply {
            self.ply = other.ply;
        }
        self.games += other.games;
        self.samples.extend(other.samples);
        while self.samples.len() > ID_SAMPLES {
            self.samples.pop_last();
        }
        self.early |= other.early;
        for (line, count) in other.lines {
            *self.lines.entry(line).or_default() += count;
        }
    }
}

fn ranked_lines(data: &RootData, depth: usize) -> Vec<(Vec<String>, u64)> {
    let mut lines: Vec<_> = data
        .lines
        .iter()
        .filter(|((d, _), _)| *d == depth)
        .map(|((_, moves), count)| (moves.clone(), *count))
        .collect();
    lines.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    lines
}

fn add_lines(line: &mut Line, data: &RootData, max_lines: usize) -> usize {
    let mut known: BTreeSet<_> = line
        .game
        .iter()
        .map(|moves| moves.iter().take(line.depth + 1).cloned().collect::<Vec<_>>())
        .collect();
    let mut added = 0;
    for (moves, _) in ranked_lines(data, line.depth).into_iter().take(max_lines) {
        if known.insert(moves.clone()) {
            line.game.push(moves);
            added += 1;
        }
    }
    if added > 0 {
        let memo = line.memo.get_or_insert_default();
        if !memo.is_empty() {
            memo.push_str("; ");
        }
        memo.push_str(&format!("seeds: game に {added} 本追加"));
    }
    added
}

fn read_updates(paths: &[PathBuf]) -> Result<BTreeMap<String, Existing>> {
    let mut updates: BTreeMap<String, Existing> = BTreeMap::new();
    for path in paths {
        read_checked_lines(path, |mut line, root, n| {
            let mut game = match updates.remove(&line.id) {
                Some(row) => {
                    // serve は id ごとに最初の行の根 (局面と手数) と own を固定し、替えた行を拒否する。
                    // 拒否される行は合わせる対象にも最後の欄の元にもしない。
                    if row.root.key != root.key
                        || row.root.ply != root.ply
                        || row.line.own != line.own
                    {
                        eprintln!(
                            "警告: {}:{n} の id={} は最初の行と根か own が違うので使いません (serve が拒否する行)",
                            path.display(),
                            line.id
                        );
                        updates.insert(line.id.clone(), row);
                        return Ok(());
                    }
                    row.line.game
                }
                None => Vec::new(),
            };
            game.append(&mut line.game);
            let mut known = BTreeSet::new();
            game.retain(|moves| known.insert(moves.clone()));
            line.game = game;
            updates.insert(
                line.id.clone(),
                Existing {
                    line,
                    root,
                    source: path.clone(),
                },
            );
            Ok(())
        })?;
    }
    updates.retain(|_, row| row.line.depth > 0);
    Ok(updates)
}

struct Existing {
    line: Line,
    root: Root,
    source: PathBuf,
}

fn update_notice(id: &str, path: &Path, added: usize) -> String {
    format!("積み直し id={id}: 元ファイル={} / 追加 {added} 本", path.display())
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
    inputs.push(("--out", &args.out));
    if let Some(path) = &args.update_out {
        reject_same_file(path, &inputs)?;
    }
    let mut updates = if args.update_out.is_some() {
        read_updates(&args.exclude_queue)?
    } else {
        BTreeMap::new()
    };
    let mut update_depths = BTreeMap::<(String, String), BTreeSet<usize>>::new();
    for row in updates.values() {
        update_depths
            .entry((row.root.key.clone(), row.line.own.clone()))
            .or_default()
            .insert(row.line.depth);
    }
    let valid: Vec<_> = args.games.iter().map(|p| valid_game_ids(p)).collect::<Result<_>>()?;
    let mut non_sweep = BTreeSet::new();
    let mut non_sweep_roots = BTreeSet::new();
    // 合流前の接頭手順ごとに採否を決めてから、各局内で根と手順を重複排除する。
    let mut prefixes: BTreeMap<(String, String, Vec<String>), u64> = BTreeMap::new();
    for (path, valid) in args.games.iter().zip(&valid) {
        visit_games(path, valid, |result, moves| {
            let Some(start) = origin(moves) else {
                return Ok(());
            };
            let origin_key = parse_root(&format!("sfen {}", moves[start].sfen_before))?.key;
            let non_black_win = result.outcome != "black_win";
            if non_black_win {
                non_sweep.insert(origin_key.clone());
            }
            for &offset in &args.offsets {
                let Some(index) = start.checked_add(offset) else {
                    continue;
                };
                if index > moves.len()
                    || (index == moves.len() && !moves.last().is_some_and(played))
                {
                    continue;
                }
                if non_black_win && let Some(root) = root_at(moves, index)? {
                    non_sweep_roots.insert(root.key);
                }
                let prefix: Vec<_> =
                    moves[start..index].iter().map(|m| m.move_usi.clone()).collect();
                for own in ["b", "w"] {
                    if own == "w" && !non_black_win {
                        continue;
                    }
                    *prefixes
                        .entry((origin_key.clone(), own.to_owned(), prefix.clone()))
                        .or_default() += 1;
                }
            }
            Ok(())
        })?;
    }
    prefixes.retain(|(origin, _, _), _| non_sweep.contains(origin));
    let mut roots: BTreeMap<(String, String), RootData> = BTreeMap::new();
    for (file, (path, valid)) in args.games.iter().zip(&valid).enumerate() {
        visit_games(path, valid, |result, moves| {
            let Some(start) = origin(moves) else {
                return Ok(());
            };
            let origin_key = parse_root(&format!("sfen {}", moves[start].sfen_before))?.key;
            if !non_sweep.contains(&origin_key) {
                return Ok(());
            }
            let early = early(result, moves, start, &args)?;
            for own in ["b", "w"] {
                if own == "w" && result.outcome == "black_win" {
                    continue;
                }
                let mut game_roots: BTreeMap<String, RootData> = BTreeMap::new();
                for &offset in &args.offsets {
                    let Some(index) = start.checked_add(offset) else {
                        continue;
                    };
                    let Some(root) = root_at(moves, index)? else {
                        continue;
                    };
                    let prefix: Vec<_> =
                        moves[start..index].iter().map(|m| m.move_usi.clone()).collect();
                    if !prefix.is_empty() {
                        // 2 回目の走査で数えた組なので必ずある。無ければ走査の間に入力が変わった。
                        let n = *prefixes
                            .get(&(origin_key.clone(), own.to_owned(), prefix))
                            .context("走査の間に棋譜が変わりました (手順の局数が見つかりません)")?;
                        if n < args.min_games_offset as u64 {
                            continue;
                        }
                    }
                    let depths = update_depths.get(&(root.key.clone(), own.to_owned()));
                    let data = game_roots.entry(root.key).or_default();
                    if data.ply == 0 || root.ply < data.ply {
                        data.ply = root.ply;
                    }
                    for depth in
                        std::iter::once(args.depth).chain(depths.into_iter().flatten().copied())
                    {
                        let line: Vec<_> = moves[index..]
                            .iter()
                            .take(depth + 1)
                            .take_while(|m| played(m))
                            .map(|m| m.move_usi.clone())
                            .collect();
                        if !line.is_empty() {
                            data.lines.insert((depth, line), 1);
                        }
                    }
                }
                for (key, mut data) in game_roots {
                    data.games = 1;
                    data.samples.insert(format!("{file}:{}", result.game_id));
                    data.early = early;
                    roots.entry((key, own.to_owned())).or_default().merge(data);
                }
            }
            Ok(())
        })?;
    }
    let excluded = excluded_positions(&args.exclude_queue)?;
    let freq = args.opp_freq.as_deref().map(open_book).transpose()?;
    let book = args.opp_book.as_deref().map(open_book).transpose()?;
    let mut freq = freq.as_ref().map(book_probe);
    let mut book = book.as_ref().map(book_probe);
    let mut excluded_counts = BTreeMap::<String, usize>::new();
    let mut picked = Vec::new();
    let mut update_counts = BTreeMap::new();
    for ((key, own), data) in roots {
        if own == "b" && !non_sweep_roots.contains(&key) {
            continue;
        }
        if let Some(ids) = excluded.get(&(key.clone(), own.clone())) {
            for id in ids {
                if let Some(row) = updates.get_mut(id)
                    && row.root.key == key
                    && row.line.own == own
                {
                    let added = add_lines(&mut row.line, &data, args.max_lines);
                    if added > 0 {
                        update_counts.insert(id.clone(), added);
                    }
                }
            }
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
        let lines = ranked_lines(&data, args.depth);
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
        let samples = data.samples.into_iter().collect::<Vec<_>>().join(",");
        writeln!(
            table,
            "{own}\t{}\t{priority}\t{class}\t{}\t{key}\t{samples};total={}\t{weight}",
            data.ply, data.games, data.games
        )?;
        *counts.entry((own, class)).or_default() += 1;
        queue.push(line);
    }
    let queue_text = format_queue(&queue)?;
    if let Some(path) = &args.update_out {
        let rows: Vec<_> = update_counts.keys().map(|id| updates[id].line.clone()).collect();
        for row in &rows {
            row.check().with_context(|| format!("積み直しの行 id={}", row.id))?;
        }
        write_atomic(path, &format_queue(&rows)?)?;
    }
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
    for (id, added) in update_counts {
        println!("{}", update_notice(&id, &updates[&id].source, added));
    }
    println!("キューの行 {}", queue.len());
    Ok(())
}

#[cfg(test)]
mod tests;
