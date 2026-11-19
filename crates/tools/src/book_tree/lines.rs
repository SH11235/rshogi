use super::journal::Reading;
use super::model::{child, side};
use super::output::NODE_MOVES;
use super::register::{is_done, reading_files};
use crate::book_db::{position_from_sfen, strip_ply};
use crate::common::dedup::canonicalize_maybe_new;
use anyhow::{Context, Result, ensure};
use clap::Args;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use serde::Deserialize;
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

struct Limits {
    depth: usize,
    cap: usize,
    stop: u32,
    draw_black: Option<i32>,
    draw_white: Option<i32>,
}

#[derive(Default)]
struct Node {
    level: Option<usize>,
    reading: Option<Reading>,
    moves: BTreeMap<String, RankedMove>,
}

struct RankedMove {
    rank: usize,
    value: i32,
    expanded: bool,
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

struct Report {
    limits: Limits,
    nodes: BTreeMap<String, Node>,
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
}

impl Reason {
    const ALL: [Self; 8] = [
        Self::Stop,
        Self::Cap,
        Self::Mate,
        Self::NoLegal,
        Self::Declaration,
        Self::Repetition,
        Self::Unexpanded,
        Self::MissingRead,
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
        }
    }
}

struct Trace {
    key: String,
    level: Option<usize>,
    moves: Vec<String>,
    reason: Reason,
    value: Option<i32>,
    merges: usize,
}

#[derive(Clone, Copy)]
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

fn set_level(node: &mut Node, key: &str, level: usize) -> Result<()> {
    ensure!(node.level.is_none_or(|known| known == level), "局面の段が一致しません: {key}");
    node.level = Some(level);
    Ok(())
}

impl Report {
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
        let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
        for path in reading_files(dir)? {
            let name = path.file_stem().and_then(|s| s.to_str()).context("読みのファイル名")?;
            let level = name[1..].parse()?;
            for line in read_lines(&path)? {
                let reading: Reading = serde_json::from_str(&line?)
                    .with_context(|| format!("{} の読みの形式が不正です", path.display()))?;
                let key = &reading.key;
                let node = nodes.entry(key.clone()).or_default();
                set_level(node, key, level)?;
                ensure!(node.reading.is_none(), "読みが重複しています: {key}");
                node.reading = Some(reading);
            }
        }

        let mut rows = read_lines(&dir.join(NODE_MOVES))?;
        ensure!(
            rows.next().transpose()?.as_deref() == Some(super::output::NODE_MOVES_HEADER),
            "{NODE_MOVES} の見出しが違います"
        );
        for row in rows {
            let row = row?;
            let cells: Vec<_> = row.split('\t').collect();
            let &[key, _, level, remaining, _, mv, value, expanded, rank] = cells.as_slice() else {
                anyhow::bail!("{NODE_MOVES} の列数が違います: {row}");
            };
            let level: usize = level.parse()?;
            let remaining: usize = remaining.parse()?;
            if remaining > 0 || level == 0 {
                let inferred = level.checked_add(remaining).context("通常深さの上限")?;
                depth = check_saved("depth", depth, Some(inferred))?;
            }
            let node = nodes.entry(key.to_owned()).or_default();
            set_level(node, key, level)?;
            ensure!(matches!(expanded, "0" | "1"), "{NODE_MOVES} の expanded が不正です: {row}");
            let ranked = RankedMove {
                rank: rank.parse()?,
                value: value.parse()?,
                expanded: expanded == "1",
            };
            ensure!(
                node.moves.insert(mv.to_owned(), ranked).is_none(),
                "手が重複しています: {row}"
            );
        }
        let depth = depth.context("通常深さの記録がありません。--depth を指定してください")?;
        let cap = depth.checked_add(extend).context("延長段数の上限")?;
        ensure!(
            nodes.values().all(|node| node.level.is_some_and(|level| level <= cap)),
            "木に depth + extend より深い読みがあります"
        );
        // 宣言勝ちや合法手のない局面には node_moves.tsv の行が無い。
        for line in read_lines(&dir.join("tree.db"))? {
            if let Some(sfen) = line?.strip_prefix("sfen ") {
                nodes.entry(strip_ply(sfen).to_owned()).or_default();
            }
        }
        Ok(Self {
            limits: Limits {
                depth,
                cap,
                stop: u32::try_from(stop)?,
                draw_black: status.draw_black,
                draw_white: status.draw_white,
            },
            nodes,
        })
    }

    fn trace(
        &self,
        start: String,
        mut path: BTreeSet<String>,
        follow: Follow,
        mut repeat_value: Option<i32>,
        mut merges: usize,
    ) -> Result<Trace> {
        let mut key = start;
        let mut moves = Vec::new();
        let (reason, value) = loop {
            if !path.insert(key.clone()) {
                break (Reason::Repetition, repeat_value);
            }
            let Some(node) = self.nodes.get(&key) else {
                break (Reason::MissingRead, None);
            };
            let Some(reading) = &node.reading else {
                break (Reason::MissingRead, None);
            };
            let level = node.level.context("読みの段がありません")?;
            if reading.bestmove == "win" {
                break (
                    Reason::Declaration,
                    Some(black(&key, reading.multipv.first().map_or(0, |pv| pv.cp))),
                );
            }
            let Some(first) = reading.lines().first() else {
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

            let (mv, value, expanded) = match follow {
                Follow::FirstRead => (
                    first.mv.as_str(),
                    first.cp,
                    node.moves.get(&first.mv).is_some_and(|row| row.expanded),
                ),
                Follow::Propagated => {
                    // node_moves.tsv は Tree::negamax と Tree::rank の結果を保存している。
                    let Some((mv, row)) = node.moves.iter().find(|(_, row)| row.rank == 1) else {
                        break (Reason::MissingRead, None);
                    };
                    (mv.as_str(), row.value, row.expanded)
                }
            };
            let next = child(&key, mv)?;
            let next_node = self.nodes.get(&next);
            let has_dependency = expanded || next_node.is_some();
            let stop = level > 0
                && first.cp.unsigned_abs() >= self.limits.stop
                && value.unsigned_abs() >= self.limits.stop;
            let cap = level >= self.limits.cap;
            let at_leaf = match follow {
                Follow::FirstRead => stop || cap,
                Follow::Propagated => !has_dependency,
            };
            if at_leaf {
                let end_value = Some(black(&key, value));
                // 保存形式では USI mate も cp に畳むため、詰みの符号化範囲で判定する。
                if (30000 - rshogi_core::types::MAX_PLY as u32..=30000)
                    .contains(&value.unsigned_abs())
                {
                    break (Reason::Mate, end_value);
                }
                if stop {
                    break (Reason::Stop, end_value);
                }
                if cap {
                    break (Reason::Cap, end_value);
                }
            }
            if !has_dependency {
                break (Reason::Unexpanded, Some(black(&key, value)));
            }
            if !path.contains(&next)
                && next_node.and_then(|n| n.level).is_some_and(|next_level| next_level <= level)
            {
                merges += 1;
            }
            repeat_value = self.limits.draw(&key);
            moves.push(mv.to_owned());
            key = next;
        };
        let level = self.nodes.get(&key).and_then(|node| node.level);
        Ok(Trace {
            key,
            level,
            moves,
            reason,
            value,
            merges,
        })
    }

    fn trace_game_move(
        &self,
        key: &str,
        mv: &str,
        next: &str,
        path: &BTreeSet<String>,
    ) -> Result<(Option<i32>, Trace)> {
        let node = self.nodes.get(key);
        let next_node = self.nodes.get(next);
        let row = node.and_then(|node| node.moves.get(mv));
        let level = next_node.and_then(|node| node.level);
        let value = row.map(|row| black(key, row.value));
        if row.is_some_and(|row| row.expanded || next_node.is_some()) {
            let merge = node
                .and_then(|n| n.level)
                .zip(level)
                .is_some_and(|(from, to)| to <= from && !path.contains(next));
            let trace = self.trace(
                next.to_owned(),
                path.clone(),
                Follow::Propagated,
                self.limits.draw(key),
                usize::from(merge),
            )?;
            return Ok((value, trace));
        }
        let reason = if node.is_some_and(|node| node.reading.is_none()) {
            Reason::MissingRead
        } else {
            Reason::Unexpanded
        };
        Ok((
            value,
            Trace {
                key: next.to_owned(),
                level,
                moves: Vec::new(),
                reason,
                value: None,
                merges: 0,
            },
        ))
    }

    fn tables(&self, paths: &Path) -> Result<(String, String, Counts)> {
        let mut counts = Counts::default();
        let mut extension = String::from(
            "start_key\tstart_level\tend_level\tplies\tend_key\tend_value_black\treason\tmoves\tmerges\n",
        );
        if self.limits.depth < self.limits.cap {
            for (key, node) in &self.nodes {
                if node.level != Some(self.limits.depth) {
                    continue;
                }
                let trace = self.trace(key.clone(), BTreeSet::new(), Follow::FirstRead, None, 0)?;
                counts.add(&trace, false);
                writeln!(
                    extension,
                    "{key}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    self.limits.depth,
                    field(trace.level),
                    trace.moves.len(),
                    trace.key,
                    field(trace.value),
                    trace.reason.name(),
                    trace.moves.join(" "),
                    trace.merges
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
            for (ply, mv) in actual.take(self.limits.depth.max(1)).enumerate() {
                path.insert(key.clone());
                let next = child(&key, mv)?;
                let (value, trace) = self.trace_game_move(&key, mv, &next, &path)?;
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

#[derive(Default)]
struct Counts {
    reasons: BTreeMap<&'static str, [usize; 2]>,
}

impl Counts {
    fn add(&mut self, trace: &Trace, game: bool) {
        let index = usize::from(game);
        self.reasons.entry(trace.reason.name()).or_default()[index] += 1;
        if trace.merges > 0 {
            self.reasons.entry("merged").or_default()[index] += 1;
        }
    }

    fn write(&self, stdout: &mut impl std::io::Write) -> Result<()> {
        for reason in Reason::ALL.map(Reason::name).into_iter().chain(["merged"]) {
            let [extension, game] = self.reasons.get(reason).copied().unwrap_or_default();
            writeln!(stdout, "{reason}\textension={extension}\tgame={game}")?;
        }
        Ok(())
    }
}

pub(super) fn run(args: LinesArgs) -> Result<()> {
    let mut destinations = BTreeSet::new();
    for output in [&args.out, &args.game_out].into_iter().flatten() {
        let normalized = canonicalize_maybe_new(output)?;
        ensure!(!normalized.try_exists()?, "既存のファイルには書きません: {}", output.display());
        ensure!(destinations.insert(normalized), "TSV の出力先が重複しています");
    }
    let report = Report::load(&args)?;
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
