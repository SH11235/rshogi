use super::journal::Reading;
use super::model::{Edges, Evaluator, NEGAMAX_EXACT_CALLS, Propagation, child, side};
use super::output::NODE_MOVES;
use super::register::{is_done, reading_files};
use crate::book_db::position_from_sfen;
use anyhow::{Context, Result, ensure};
use clap::Args;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub(super) struct LinesArgs {
    /// 読み終わった木の出力先
    #[arg(long)]
    dir: PathBuf,
    /// 延長線の TSV (省略時は標準出力)
    #[arg(long)]
    out: Option<PathBuf>,
    /// 実戦手の TSV (省略時は標準出力)
    #[arg(long)]
    game_out: Option<PathBuf>,
    /// 保存値を使えないときの通常深さ
    #[arg(long)]
    depth: Option<usize>,
    /// 保存値が無いときの延長段数
    #[arg(long)]
    extend: Option<usize>,
    /// 保存値が無いときの打ち切り値 (省略時は 300)
    #[arg(long, value_parser = clap::value_parser!(i32).range(0..=30000))]
    stop: Option<i32>,
    /// paths.record.tsv が無いときの実戦手順 (paths.tsv 形式)
    #[arg(long)]
    paths: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Status {
    ready: bool,
    depth: Option<usize>,
    extend: Option<usize>,
    stop: Option<i32>,
    draw_black: Option<i32>,
    draw_white: Option<i32>,
}

/// 実戦手の追跡の使い回しで覚えておく件数の上限。
const TRACE_CACHE_LIMIT: usize = 4096;

struct Limits {
    depth: usize,
    cap: usize,
    stop: u32,
    draw_black: Option<i32>,
    draw_white: Option<i32>,
}

impl Limits {
    fn draw(&self, key: &str) -> Option<i32> {
        if side(key) == "b" {
            self.draw_black
        } else {
            self.draw_white.map(|value| -value)
        }
    }
}

type TraceKey = (String, String, BTreeSet<String>);
type GameTrace = (Option<i32>, Trace);

struct Report<'a> {
    traces: RefCell<BTreeMap<TraceKey, GameTrace>>,
    limits: Limits,
    levels: Cow<'a, BTreeMap<String, usize>>,
    readings: Cow<'a, BTreeMap<String, Reading>>,
    expanded: Cow<'a, Edges>,
    start_values: BTreeMap<String, i32>,
    extend_unsaved: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reason {
    Stop,
    Cap,
    Mate,
    NoLegal,
    Declaration,
    Repetition,
    Unexpanded,
    MissingRead,
    Merged,
}

impl Reason {
    const ALL: [Self; 9] = [
        Self::Stop,
        Self::Cap,
        Self::Mate,
        Self::NoLegal,
        Self::Declaration,
        Self::Repetition,
        Self::Unexpanded,
        Self::MissingRead,
        Self::Merged,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Cap => "cap",
            Self::Mate => "mate",
            Self::NoLegal => "no-legal",
            Self::Declaration => "declaration",
            Self::Repetition => "repetition",
            Self::Unexpanded => "unexpanded",
            Self::MissingRead => "missing-read",
            Self::Merged => "merged",
        }
    }
}

#[derive(Clone)]
struct Trace {
    start_value: Option<i32>,
    key: String,
    level: Option<usize>,
    moves: Vec<String>,
    reason: Reason,
    value: Option<i32>,
    merges: usize,
    deviates: bool,
    pending: Option<(String, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Follow {
    FirstRead,
    Propagated,
}

fn black(key: &str, value: i32) -> i32 {
    if side(key) == "w" { -value } else { value }
}

fn field(value: Option<impl ToString>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn check_saved<T: Copy + Eq>(
    name: &str,
    supplied: Option<T>,
    saved: Option<T>,
) -> Result<Option<T>> {
    ensure!(
        supplied.zip(saved).is_none_or(|(a, b)| a == b),
        "--{name} が木の記録と一致しません"
    );
    Ok(saved.or(supplied))
}

fn read_lines(path: &Path) -> Result<std::io::Lines<BufReader<File>>> {
    Ok(BufReader::new(
        File::open(path).with_context(|| format!("{} を開けません", path.display()))?,
    )
    .lines())
}

fn set_level(levels: &mut BTreeMap<String, usize>, key: &str, level: usize) -> Result<()> {
    ensure!(
        levels.insert(key.to_owned(), level).is_none_or(|known| known == level),
        "局面の段が一致しません: {key}"
    );
    Ok(())
}

fn read_end(value: i32, stop: bool, cap: bool) -> Reason {
    // USI mate と同じ値域の cp は、保存した値から区別できない。
    if (30000 - rshogi_core::types::MAX_PLY as u32..=30000).contains(&value.unsigned_abs()) {
        Reason::Mate
    } else if stop {
        Reason::Stop
    } else if cap {
        Reason::Cap
    } else {
        Reason::Unexpanded
    }
}

impl Report<'_> {
    fn load(args: &LinesArgs) -> Result<Self> {
        let dir = &args.dir;
        ensure!(
            is_done(&fs::read_to_string(dir.join("STATUS"))?),
            "STATUS が DONE ではありません"
        );
        let status: Status =
            serde_json::from_reader(File::open(dir.join("registration_status.json"))?)?;
        ensure!(status.ready, "木が ready ではありません");

        let mut depth = check_saved("depth", args.depth, status.depth)?;
        let extend = check_saved("extend", args.extend, status.extend)?
            .context("延長段数の記録がありません。--extend を指定してください")?;
        let stop = check_saved("stop", args.stop, status.stop)?.unwrap_or(300);
        let mut levels = BTreeMap::new();
        let mut readings = BTreeMap::new();
        for path in reading_files(dir)? {
            let name = path.file_stem().and_then(|s| s.to_str()).context("読みのファイル名")?;
            let level = name[1..].parse()?;
            for line in read_lines(&path)? {
                let reading: Reading = serde_json::from_str(&line?)
                    .with_context(|| format!("{} の読みの形式が不正です", path.display()))?;
                let key = &reading.key;
                set_level(&mut levels, key, level)?;
                ensure!(!readings.contains_key(key), "読みが重複しています: {key}");
                readings.insert(key.clone(), reading);
            }
        }

        let mut rows = read_lines(&dir.join(NODE_MOVES))?;
        ensure!(
            rows.next().transpose()?.as_deref() == Some(super::output::NODE_MOVES_HEADER),
            "{NODE_MOVES} の見出しが違います"
        );
        let mut expanded = Edges::new();
        let mut start_values = BTreeMap::new();
        for row in rows {
            let row = row?;
            let cells: Vec<_> = row.split('\t').collect();
            let &[key, _, level, remaining, _, mv, value, expanded_flag, rank] = cells.as_slice()
            else {
                anyhow::bail!("{NODE_MOVES} の列数が違います: {row}");
            };
            let level: usize = level.parse()?;
            let remaining: usize = remaining.parse()?;
            if remaining > 0 || level == 0 {
                let inferred = level.checked_add(remaining).context("通常深さの上限")?;
                depth = check_saved("depth", depth, Some(inferred))?;
            }
            set_level(&mut levels, key, level)?;
            let value = value.parse()?;
            if rank.parse::<usize>()? == 1 {
                ensure!(
                    start_values.insert(key.to_owned(), black(key, value)).is_none(),
                    "1 位の手が重複しています: {row}"
                );
            }
            match expanded_flag {
                "0" => {}
                "1" => {
                    ensure!(
                        expanded.entry(key.to_owned()).or_default().insert(mv.to_owned()),
                        "手が重複しています: {row}"
                    );
                }
                _ => anyhow::bail!("{NODE_MOVES} の expanded が不正です: {row}"),
            }
        }
        let depth = depth.context("通常深さの記録がありません。--depth を指定してください")?;
        let cap = depth.checked_add(extend).context("延長段数の上限")?;
        let mandatory_children: BTreeSet<_> = readings
            .values()
            .filter(|parent| parent.structural_move().is_some())
            .flat_map(|parent| {
                expanded
                    .get(&parent.key)
                    .into_iter()
                    .flatten()
                    .map(|mv| child(&parent.key, mv).map(|key| (key, levels[&parent.key] + 1)))
            })
            .collect::<Result<_>>()?;
        ensure!(
            levels
                .iter()
                .all(|(key, &level)| level <= cap
                    || mandatory_children.contains(&(key.clone(), level))),
            "木に唯一手の延長でない depth + extend より深い読みがあります"
        );
        Ok(Self {
            traces: RefCell::default(),
            limits: Limits {
                depth,
                cap,
                stop: u32::try_from(stop)?,
                draw_black: status.draw_black,
                draw_white: status.draw_white,
            },
            levels: Cow::Owned(levels),
            readings: Cow::Owned(readings),
            expanded: Cow::Owned(expanded),
            start_values,
            extend_unsaved: status.extend.is_none(),
        })
    }

    fn propagation(&self) -> Result<Propagation<'_>> {
        Propagation::new(
            &self.readings,
            &self.expanded,
            &self.readings.keys().cloned().collect(),
            [
                self.limits.draw_black.unwrap_or(0),
                self.limits.draw_white.unwrap_or(0),
            ],
        )
    }

    fn deepest_read(&self) -> Option<usize> {
        self.readings.keys().map(|key| self.levels[key]).max()
    }

    fn trace(
        &self,
        propagation: &mut Evaluator<'_>,
        mut key: String,
        mut path: BTreeSet<String>,
        follow: Follow,
        mut merges: usize,
    ) -> Result<Trace> {
        let mut moves = Vec::new();
        let mut start_value = None;
        let mut deviates = false;
        let mut pending = None;
        let (reason, value) = loop {
            let Some(reading) = self.readings.get(&key) else {
                break (Reason::MissingRead, None);
            };
            let level = self.levels[&key];
            let position =
                propagation.position(&key, &mut path).context("伝播の読みがありません")?;
            start_value.get_or_insert(black(&key, position.value));
            if reading.bestmove == "win" {
                break (
                    Reason::Declaration,
                    Some(black(&key, reading.multipv.first().map_or(0, |pv| pv.cp))),
                );
            }
            let first = reading.lines().first();
            let chosen = match follow {
                Follow::FirstRead => {
                    reading.structural_move().or_else(|| first.map(|pv| pv.mv.as_str()))
                }
                Follow::Propagated => position.best.as_deref(),
            };
            let Some(mv) = chosen else {
                let pos = position_from_sfen(&format!("{key} 1"))?;
                let mut legal = MoveList::new();
                generate_legal_all(&pos, &mut legal);
                break if legal.is_empty() {
                    (
                        if pos.in_check() {
                            Reason::Mate
                        } else {
                            Reason::NoLegal
                        },
                        Some(0),
                    )
                } else {
                    (Reason::MissingRead, None)
                };
            };
            let value = position.rows[mv];
            let stop =
                level > 0 && first.is_some_and(|pv| pv.cp.unsigned_abs() >= self.limits.stop);
            let cap = level >= self.limits.cap;
            if follow == Follow::FirstRead {
                deviates |= position.best.as_deref() != Some(mv);
                if let Some(first) = first.filter(|_| stop || cap) {
                    break (read_end(first.cp, stop, cap), Some(black(&key, first.cp)));
                }
            }

            let next = child(&key, mv)?;
            path.insert(key.clone());
            if path.contains(&next) {
                let draw = self.limits.draw(&key);
                moves.push(mv.to_owned());
                key = next;
                break (Reason::Repetition, draw);
            }
            let expanded = self.expanded.get(&key).is_some_and(|edges| edges.contains(mv));
            if !expanded && !self.readings.contains_key(&next) {
                pending = Some((key.clone(), mv.to_owned()));
                break (
                    read_end(value, stop && value.unsigned_abs() >= self.limits.stop, cap),
                    Some(black(&key, value)),
                );
            }
            let merged = self.readings.contains_key(&next) && self.levels[&next] <= level;
            merges += usize::from(merged);
            moves.push(mv.to_owned());
            let value = black(&key, value);
            key = next;
            if merged && follow == Follow::FirstRead {
                break (Reason::Merged, Some(value));
            }
        };
        let level = self.levels.get(&key).copied();
        Ok(Trace {
            start_value,
            key,
            level,
            moves,
            reason,
            value,
            merges,
            deviates,
            pending,
        })
    }

    fn trace_game_move(
        &self,
        propagation: &mut Evaluator<'_>,
        key: &str,
        mv: &str,
        next: &str,
        path: &BTreeSet<String>,
    ) -> Result<(Option<i32>, Trace)> {
        let cache_key = (key.to_owned(), mv.to_owned(), path.clone());
        if let Some(result) = self.traces.borrow().get(&cache_key) {
            return Ok(result.clone());
        }
        let result = self.trace_game_move_uncached(propagation, key, mv, next, path)?;
        let mut traces = self.traces.borrow_mut();
        // 鍵は祖先の手順を丸ごと持つので、実戦の手順が多い入力でメモリが増え続けないよう上限で捨てる。
        if traces.len() >= TRACE_CACHE_LIMIT {
            traces.clear();
        }
        traces.insert(cache_key, result.clone());
        Ok(result)
    }

    fn trace_game_move_uncached(
        &self,
        propagation: &mut Evaluator<'_>,
        key: &str,
        mv: &str,
        next: &str,
        path: &BTreeSet<String>,
    ) -> Result<GameTrace> {
        let reading = self.readings.get(key);
        let source_level = self.levels[key];
        let raw = reading.and_then(|r| r.lines().iter().find(|pv| pv.mv == mv));
        let expanded = self.expanded.get(key).is_some_and(|edges| edges.contains(mv));
        let next_level = self.levels.get(next).copied();
        let raw_value = raw.map(|pv| black(key, pv.cp));
        let (value, reason, end_key, end_level, end_value) = if path.contains(next) {
            let draw = self.limits.draw(key);
            (draw, Reason::Repetition, next, next_level, draw)
        } else if (raw.is_some() && self.readings.contains_key(next)) || expanded {
            let ancestors = path.clone();
            let merges = usize::from(next_level.is_some_and(|level| level <= source_level));
            let trace =
                self.trace(propagation, next.to_owned(), ancestors, Follow::Propagated, merges)?;
            return Ok((trace.start_value.or(raw_value), trace));
        } else if let Some(cp) = (source_level > 0)
            .then(|| {
                // 1 位が STOP 以上でも実戦手の値は決まらないので、決着と言えるのは実戦手自身が
                // STOP 以上か、1 位が -STOP 以下 (どの手も負け) のときだけ。
                raw.map(|pv| pv.cp).filter(|cp| cp.unsigned_abs() >= self.limits.stop).or_else(
                    || {
                        reading
                            .and_then(|r| r.lines().first())
                            .map(|pv| pv.cp)
                            .filter(|&cp| cp <= -(self.limits.stop as i32))
                    },
                )
            })
            .flatten()
        {
            (
                raw_value,
                read_end(cp, true, false),
                key,
                Some(source_level),
                Some(black(key, cp)),
            )
        } else if source_level >= self.limits.cap {
            (raw_value, Reason::Cap, key, Some(source_level), raw_value)
        } else {
            let reason = if reading.is_some() {
                Reason::Unexpanded
            } else {
                Reason::MissingRead
            };
            (raw_value, reason, next, next_level, raw_value)
        };
        Ok((
            value,
            Trace {
                start_value: value,
                key: end_key.to_owned(),
                level: end_level,
                moves: Vec::new(),
                reason,
                value: end_value,
                merges: 0,
                deviates: false,
                pending: Some((key.to_owned(), mv.to_owned())),
            },
        ))
    }

    fn tables(&self, paths: &Path) -> Result<(String, String, Counts)> {
        let graph = self.propagation()?;
        let mut propagation = graph.evaluator(NEGAMAX_EXACT_CALLS);
        let mut counts = Counts::default();
        let mut extension = String::from(
            "start_key\tstart_level\tend_level\tplies\tend_key\tend_value_black\treason\tmoves\tmerges\tstart_propagated_black\tdeviates\n",
        );
        if self.limits.depth < self.limits.cap {
            for (key, &level) in self.levels.iter() {
                if level != self.limits.depth {
                    continue;
                }
                let start_value = self.start_values.get(key).copied().or_else(|| {
                    propagation.position(key, &mut BTreeSet::new()).map(|p| black(key, p.value))
                });
                let trace = self.trace(
                    &mut propagation,
                    key.clone(),
                    BTreeSet::new(),
                    Follow::FirstRead,
                    0,
                )?;
                counts.add(&trace, false);
                writeln!(
                    extension,
                    "{key}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    self.limits.depth,
                    field(trace.level),
                    trace.moves.len(),
                    trace.key,
                    field(trace.value),
                    trace.reason.name(),
                    trace.moves.join(" "),
                    trace.merges,
                    field(start_value),
                    u8::from(trace.deviates)
                )?;
            }
        }
        let mut game = String::from(
            "game_index\tply_in_game\tmove\tpropagated_value_black\tline_end_level\treason\tend_value_black\tplies\tmoves\tmerges\n",
        );
        let mut game_index = 0;
        for row in read_lines(paths)? {
            let row = row?;
            if row.trim().is_empty() {
                continue;
            }
            let mut cols = row.split('\t');
            let root = cols.next().context("paths の key がありません")?;
            let _: u32 = cols.next().context("paths の手数がありません")?.parse()?;
            let mut actual =
                cols.next().context("paths の手順がありません")?.split_whitespace().peekable();
            if actual.peek().is_none() {
                continue;
            }
            game_index += 1;
            let mut key = root.to_owned();
            let mut path = BTreeSet::new();
            for (ply, mv) in actual.enumerate() {
                path.insert(key.clone());
                let next = child(&key, mv)?;
                if !self.levels.contains_key(&key) {
                    key = next;
                    continue;
                }
                let (value, trace) =
                    self.trace_game_move(&mut propagation, &key, mv, &next, &path)?;
                counts.add(&trace, true);
                writeln!(
                    game,
                    "{game_index}\t{}\t{mv}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    ply + 1,
                    field(value),
                    field(trace.level),
                    trace.reason.name(),
                    field(trace.value),
                    trace.moves.len(),
                    trace.moves.join(" "),
                    trace.merges
                )?;
                key = next;
            }
        }
        Ok((extension, game, counts))
    }
}

#[derive(Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Unsettled {
    pub start_key: String,
    pub start_move: String,
    pub end_key: String,
    pub end_move: String,
    pub reason: &'static str,
}

/// 実戦と根の比較手から、祖先を含む伝播線の不足辺を集める。
pub(super) fn unsettled(tree: &super::model::Tree) -> Result<Vec<Unsettled>> {
    let report = Report {
        traces: RefCell::default(),
        limits: Limits {
            depth: tree.def.depth,
            cap: tree.def.max_depth(),
            stop: tree.def.stop as u32,
            draw_black: Some(tree.def.draw_black.unwrap_or(tree.def.draw)),
            draw_white: Some(tree.def.draw_white.unwrap_or(tree.def.draw)),
        },
        levels: Cow::Borrowed(&tree.level),
        readings: Cow::Borrowed(&tree.nodes),
        expanded: Cow::Borrowed(&tree.edge),
        start_values: BTreeMap::new(),
        extend_unsaved: false,
    };
    let graph = report.propagation()?;
    let mut propagation = graph.evaluator(NEGAMAX_EXACT_CALLS);
    let mut starts = Vec::new();
    for key in tree.roots.keys() {
        let mut moves: BTreeSet<_> =
            tree.candidates.get(key).into_iter().flatten().cloned().collect();
        if let Some(position) = propagation.position(key, &mut BTreeSet::new()) {
            moves.extend(position.best);
        }
        for mv in moves {
            starts.push((key.clone(), mv));
        }
    }
    let mut pending = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut trace_move = |key: &str, mv: &str, path: &BTreeSet<String>| -> Result<()> {
        if !seen.insert((key.to_owned(), mv.to_owned(), path.clone())) {
            return Ok(());
        }
        let next = child(key, mv)?;
        let (_, trace) = report.trace_game_move(&mut propagation, key, mv, &next, path)?;
        if trace.reason == Reason::Unexpanded
            && let Some((end_key, end_move)) = trace.pending
            && tree.level[&end_key] < tree.def.max_depth()
        {
            pending.insert(Unsettled {
                start_key: key.to_owned(),
                start_move: mv.to_owned(),
                end_key,
                end_move,
                reason: "unexpanded",
            });
        }
        Ok(())
    };
    for (key, mv) in starts {
        trace_move(&key, &mv, &BTreeSet::from([key.clone()]))?;
    }
    for (root, _, moves) in &tree.game_lines {
        let mut key = root.clone();
        let mut path = BTreeSet::new();
        for mv in moves {
            path.insert(key.clone());
            if tree.nodes.contains_key(&key) {
                trace_move(&key, mv, &path)?;
            }
            key = child(&key, mv)?;
        }
    }
    Ok(pending.into_iter().collect())
}

#[derive(Default)]
struct Counts {
    reasons: BTreeMap<&'static str, [usize; 2]>,
}

impl Counts {
    fn add(&mut self, trace: &Trace, game: bool) {
        let index = usize::from(game);
        self.reasons.entry(trace.reason.name()).or_default()[index] += 1;
        if game && trace.merges > 0 {
            self.reasons.entry(Reason::Merged.name()).or_default()[index] += 1;
        }
    }

    fn write(&self, stdout: &mut impl std::io::Write) -> Result<()> {
        for reason in Reason::ALL.map(Reason::name) {
            let [extension, game] = self.reasons.get(reason).copied().unwrap_or_default();
            writeln!(stdout, "{reason}\textension={extension}\tgame={game}")?;
        }
        Ok(())
    }
}

pub(super) fn run(mut args: LinesArgs) -> Result<()> {
    args.dir = args.dir.canonicalize()?;
    let mut destinations = BTreeSet::new();
    for output in [&mut args.out, &mut args.game_out].into_iter().flatten() {
        let parent = super::paths::parent_dir(output).canonicalize()?;
        ensure!(
            !parent.starts_with(&args.dir),
            "木のディレクトリ内には出力できません: {}",
            output.display()
        );
        let name = output.file_name().context("TSV の出力ファイル名がありません")?;
        let normalized = parent.join(name);
        ensure!(
            !normalized.try_exists()? && !normalized.is_symlink(),
            "既存のファイルには書きません: {}",
            output.display()
        );
        ensure!(destinations.insert(normalized.clone()), "TSV の出力先が重複しています");
        *output = normalized;
    }
    let report = Report::load(&args)?;
    if report.extend_unsaved
        && let Some(deepest) = report.deepest_read()
        && deepest < report.limits.cap
    {
        eprintln!(
            "警告: 最深の読みは {deepest} 段で、指定した上限 {} 段に達していません。--extend が実際より大きい可能性があります",
            report.limits.cap
        );
    }
    let paths = args.paths.unwrap_or_else(|| args.dir.join("paths.record.tsv"));
    let (extension, game, counts) = report.tables(&paths)?;
    let mut stdout = std::io::stdout().lock();
    for (output, table) in [(args.out, extension), (args.game_out, game)] {
        if let Some(output) = output {
            let mut temp = tempfile::NamedTempFile::new_in(super::paths::parent_dir(&output))?;
            temp.write_all(table.as_bytes())?;
            temp.persist_noclobber(&output)
                .with_context(|| format!("{} に書けません", output.display()))?;
        } else {
            stdout.write_all(table.as_bytes())?;
        }
    }
    counts.write(&mut stdout)
}

#[cfg(test)]
mod tests;
