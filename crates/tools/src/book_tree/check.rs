//! 書き出した定跡とプリセットの整合性・到達局面・循環への入口を検査する。

use super::journal::digest;
use super::reply_graph::{
    self, BookIndex, DirectedGraph, HIRATE_KEY, MoveSource, NONE, ReplyGraph,
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub(super) struct CheckArgs {
    /// 書き出した定跡 (.db)
    db: PathBuf,
    /// 手の role・evals・read を持つ注釈 (.annot.json)
    annotation: PathBuf,
    /// 指す手のプリセット (.explore.txt)
    preset: PathBuf,
    /// 自分の手番の局面で、この値以下を報告する
    #[arg(long, default_value_t = -150.0, allow_hyphen_values = true)]
    bad_eval: f64,
    /// グラフ構築のスレッド数 (0 = 全コア)
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

#[derive(Deserialize)]
struct Annotation {
    book: AnnotationBook,
    positions: BTreeMap<String, AnnotationPosition>,
}

#[derive(Deserialize)]
struct AnnotationBook {
    sha256: String,
}

#[derive(Deserialize)]
struct AnnotationPosition {
    moves: BTreeMap<String, AnnotationMove>,
    read: Option<Reading>,
}

#[derive(Deserialize)]
struct AnnotationMove {
    role: String,
    evals: Vec<f64>,
}

#[derive(Deserialize)]
struct Reading {
    main: Option<f64>,
    top: Option<f64>,
}

impl AnnotationPosition {
    fn value(&self) -> (Option<f64>, &'static str) {
        if let Some(value) = self.read.as_ref().and_then(|r| r.main) {
            return (Some(value), "main");
        }
        if let Some(main) = self.moves.values().find(|m| m.role == "main")
            && !main.evals.is_empty()
        {
            return (Some(main.evals.iter().sum::<f64>() / main.evals.len() as f64), "evals");
        }
        (self.read.as_ref().and_then(|r| r.top), "top")
    }
}

type Selections = BTreeMap<String, BTreeSet<String>>;

fn read_preset(reader: impl BufRead) -> Result<Selections> {
    let mut selections = Selections::new();
    for (number, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() >= 5 && fields[0] == "sfen", "preset:{}: 不正な行", number + 1);
        selections
            .entry(fields[1..4].join(" "))
            .or_default()
            .extend(fields[4..].iter().map(|m| (*m).to_owned()));
    }
    Ok(selections)
}

fn validate_db(reader: impl BufRead, annot: &Annotation) -> Result<()> {
    // 通常の定跡 reader は重複を併合するため、照合では元の行の重複も検査する。
    let mut seen = BTreeSet::new();
    let mut current: Option<(&str, BTreeSet<String>)> = None;
    let finish = |entry: Option<(&str, BTreeSet<String>)>| -> Result<()> {
        if let Some((key, moves)) = entry {
            ensure!(
                !moves.is_empty() && moves.iter().eq(annot.positions[key].moves.keys()),
                "annotation and DB move sets differ: {key}"
            );
        }
        Ok(())
    };
    for (number, line) in reader.lines().enumerate() {
        let line = line?;
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.is_empty() || line.starts_with('#') {
            continue;
        }
        ensure!(fields.len() == 5, "DB:{}: 不正な行", number + 1);
        if fields[0] == "sfen" {
            finish(current.take())?;
            let key = fields[1..4].join(" ");
            let (key, _) = annot
                .positions
                .get_key_value(&key)
                .with_context(|| format!("annotation and DB move sets differ: {key}"))?;
            ensure!(seen.insert(key.as_str()), "DB: duplicate position: {key}");
            current = Some((key, BTreeSet::new()));
        } else {
            let (key, moves) = current.as_mut().context("DB: 局面の前に手があります")?;
            ensure!(moves.insert(fields[0].to_owned()), "DB: duplicate move: {key} {}", fields[0]);
        }
    }
    finish(current)?;
    ensure!(seen.len() == annot.positions.len(), "annotation and DB move sets differ");
    Ok(())
}

fn membership(annot: &Annotation, selections: &Selections, out: &mut impl Write) -> Result<usize> {
    let mut error_details = Vec::new();
    let mut record_details = Vec::new();
    let mut errors = 0;
    let mut records = 0;
    let mut record_positions = 0;
    for (key, moves) in selections {
        let Some(position) = annot.positions.get(key) else {
            writeln!(error_details, "  unknown position: {key}")?;
            errors += 1;
            continue;
        };
        let mut has_record = false;
        for mv in moves {
            match position.moves.get(mv) {
                None => {
                    writeln!(error_details, "  unknown move: {key} | {mv}")?;
                    errors += 1;
                }
                Some(m) if m.role == "record" => {
                    writeln!(record_details, "  record selection: {key} | {mv}")?;
                    records += 1;
                    has_record = true;
                }
                Some(_) => {}
            }
        }
        record_positions += usize::from(has_record);
    }
    writeln!(out, "preset membership errors: {errors}")?;
    out.write_all(&error_details)?;
    writeln!(out, "preset record selections: {records} positions={record_positions}")?;
    out.write_all(&record_details)?;
    Ok(errors)
}

impl MoveSource for AnnotationPosition {
    fn move_text(&self, i: usize) -> &str {
        self.moves.keys().nth(i).expect("注釈の手の番号")
    }
}

fn select(position: &AnnotationPosition, selection: Option<&BTreeSet<String>>) -> Vec<usize> {
    position
        .moves
        .iter()
        .enumerate()
        .filter(|(_, (mv, m))| selection.map_or(m.role == "main", |s| s.contains(*mv)))
        .map(|(i, _)| i)
        .collect()
}

struct Traversal {
    parent: Vec<u32>,
    parent_move: Vec<u32>,
    last_own: Vec<u32>,
}

impl Traversal {
    fn new(graph: &ReplyGraph<'_, AnnotationPosition>) -> Self {
        let mut parent = vec![NONE; graph.len()];
        let mut parent_move = vec![NONE; graph.len()];
        let mut last_own = vec![NONE; graph.len()];
        let mut seen = vec![false; graph.len()];
        let mut queue = VecDeque::from([0]);
        seen[0] = true;
        while let Some(v) = queue.pop_front() {
            for (i, e) in graph.edges(v).iter().enumerate() {
                let w = e.to();
                if !seen[w] {
                    seen[w] = true;
                    parent[w] = v as u32;
                    parent_move[w] = i as u32;
                    last_own[w] = if graph.own[v] { w as u32 } else { last_own[v] };
                    queue.push_back(w);
                }
            }
        }
        Self {
            parent,
            parent_move,
            last_own,
        }
    }

    fn path(&self, graph: &ReplyGraph<'_, AnnotationPosition>, mut v: usize) -> String {
        let mut moves = Vec::new();
        while self.parent[v] != NONE {
            let p = self.parent[v] as usize;
            let i = self.parent_move[v] as usize;
            moves.push(graph.move_text(p, i));
            v = p;
        }
        moves.reverse();
        moves.join(" ")
    }
}

#[derive(Default)]
struct EvaluationCounts {
    bad: usize,
    unknown: usize,
    reachable: usize,
}

fn general(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("指数表記");
    let exponent: i32 = exponent.parse().expect("指数");
    if !(-4..6).contains(&exponent) {
        format!("{}e{exponent:+03}", mantissa.trim_end_matches('0').trim_end_matches('.'))
    } else {
        let text = format!("{:.*}", (5 - exponent).max(0) as usize, value);
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            text
        }
    }
}

fn evaluations(
    graph: &ReplyGraph<'_, AnnotationPosition>,
    index: &BookIndex<'_, AnnotationPosition>,
    annot: &Annotation,
    traversal: &Traversal,
    side: &str,
    threshold: f64,
    out: &mut impl Write,
) -> Result<EvaluationCounts> {
    let mut counts = EvaluationCounts::default();
    for v in 0..graph.len() {
        if !graph.own[v] {
            continue;
        }
        let Some(m) = graph.master(v) else {
            continue;
        };
        let key = index.key(m);
        counts.reachable += 1;
        let (value, basis) = annot.positions[key].value();
        let Some(value) = value else {
            counts.unknown += 1;
            continue;
        };
        if value <= threshold {
            counts.bad += 1;
            writeln!(out, "  side={side} eval={} basis={basis} key={key}", general(value))?;
            if traversal.last_own[v] != NONE {
                let child = traversal.last_own[v] as usize;
                let p = traversal.parent[child] as usize;
                let i = traversal.parent_move[child] as usize;
                let key = graph.key(p)?;
                let mv = graph.move_text(p, i);
                let role = &annot.positions[&key].moves[mv.as_ref()].role;
                writeln!(out, "    last own move (key, move, role): ({key}, {mv}, {role})")?;
            } else {
                writeln!(out, "    last own move (key, move, role): None")?;
            }
            writeln!(out, "    startpos moves {}", traversal.path(graph, v))?;
        }
    }
    Ok(counts)
}

struct Cycles {
    components: Vec<Vec<u32>>,
    component: Vec<u32>,
}

impl Cycles {
    fn new(adjacency: &(impl DirectedGraph + ?Sized)) -> Self {
        let mut components = reply_graph::nontrivial_sccs_graph(adjacency);
        let mut component = vec![NONE; adjacency.len()];
        for (i, c) in components.iter().enumerate() {
            for &v in c {
                component[v as usize] = i as u32;
            }
        }
        for (v, c) in component.iter_mut().enumerate() {
            if *c == NONE && (0..adjacency.degree(v)).any(|i| adjacency.target(v, i) == v as u32) {
                *c = components.len() as u32;
                components.push(vec![v as u32]);
            }
        }
        Self {
            components,
            component,
        }
    }

    fn contains(&self, v: usize) -> bool {
        self.component[v] != NONE
    }
}

fn add_count(to: &mut u128, n: u128) -> Result<()> {
    *to = to.checked_add(n).context("手順数が u128 の上限を超えました")?;
    Ok(())
}

#[derive(Debug, Default, PartialEq)]
struct Distribution {
    exits: Vec<u128>,
    entries: Vec<u128>,
    total: u128,
    cycle_entries: u128,
}

fn distribution(
    adjacency: &(impl DirectedGraph + ?Sized),
    cycles: &Cycles,
) -> Result<Distribution> {
    let mut result = Distribution::default();
    let n = adjacency.len();
    if n == 0 {
        return Ok(result);
    }
    // 手数ごとに二面を入れ替え、各局面に全手数分の分布を保持しない。
    let mut current = vec![0u128; n];
    let mut next = vec![0u128; n];
    let mut active = vec![0usize];
    let mut next_active = Vec::new();
    current[0] = 1;
    while !active.is_empty() {
        let mut exits = 0;
        let mut entries = 0;
        for v in active.drain(..) {
            let count = std::mem::take(&mut current[v]);
            if cycles.contains(v) {
                add_count(&mut entries, count)?;
            } else if adjacency.degree(v) == 0 {
                add_count(&mut exits, count)?;
            } else {
                for i in 0..adjacency.degree(v) {
                    let w = adjacency.target(v, i) as usize;
                    if next[w] == 0 {
                        next_active.push(w);
                    }
                    add_count(&mut next[w], count)?;
                }
            }
        }
        add_count(&mut result.total, exits)?;
        add_count(&mut result.total, entries)?;
        add_count(&mut result.cycle_entries, entries)?;
        result.exits.push(exits);
        result.entries.push(entries);
        std::mem::swap(&mut current, &mut next);
        std::mem::swap(&mut active, &mut next_active);
    }
    Ok(result)
}

const CYCLE_WORK_LIMIT: usize = 10_000_000;

#[derive(Debug, PartialEq)]
enum ShortestCycle {
    Found(u8),
    AtMost(u8),
    Longer,
    Unknown,
}

impl ShortestCycle {
    fn labels(&self) -> (String, &'static str) {
        match self {
            Self::Found(n) => (n.to_string(), "true"),
            Self::AtMost(n) => (format!("<={n}"), "true"),
            Self::Longer => (">16".into(), "false"),
            Self::Unknown => ("unknown".into(), "unknown"),
        }
    }
}

fn shortest_cycle(
    adjacency: &(impl DirectedGraph + ?Sized),
    cycles: &Cycles,
    id: usize,
    distance: &mut [u8],
    queue: &mut Vec<u32>,
    mut budget: usize,
) -> ShortestCycle {
    let mut best = 17;
    for &root in &cycles.components[id] {
        queue.clear();
        queue.push(root);
        distance[root as usize] = 0;
        let mut head = 0;
        let mut exhausted = false;
        while head < queue.len() {
            if budget == 0 {
                exhausted = true;
                break;
            }
            budget -= 1;
            let v = queue[head] as usize;
            head += 1;
            let length = distance[v] + 1;
            if length >= best {
                continue;
            }
            for i in 0..adjacency.degree(v) {
                if budget == 0 {
                    exhausted = true;
                    break;
                }
                budget -= 1;
                let w = adjacency.target(v, i) as usize;
                if cycles.component[w] != id as u32 {
                    continue;
                }
                if w == root as usize {
                    best = length;
                    break;
                }
                if length < 16 && distance[w] == u8::MAX {
                    distance[w] = length;
                    queue.push(w as u32);
                }
            }
            if exhausted {
                break;
            }
        }
        for &v in queue.iter() {
            distance[v as usize] = u8::MAX;
        }
        if exhausted {
            return if best <= 16 {
                ShortestCycle::AtMost(best)
            } else {
                ShortestCycle::Unknown
            };
        }
        if best == 1 {
            return ShortestCycle::Found(1);
        }
    }
    if best <= 16 {
        ShortestCycle::Found(best)
    } else {
        ShortestCycle::Longer
    }
}

fn paths(
    graph: &ReplyGraph<'_, AnnotationPosition>,
    traversal: &Traversal,
    side: &str,
    out: &mut impl Write,
) -> Result<()> {
    let cycles = Cycles::new(graph);
    let counts = distribution(graph, &cycles)?;
    if cycles.components.is_empty() {
        writeln!(out, "paths {side}: total={}, repetitions=0", counts.total)?;
    } else {
        writeln!(
            out,
            "paths {side}: cycle-entry cutoff total={}, cycle-entries={}",
            counts.total, counts.cycle_entries
        )?;
    }
    writeln!(
        out,
        "  ply = moves played from startpos; opponent moves outside the book are excluded"
    )?;
    for (ply, (&exits, &entries)) in counts.exits.iter().zip(&counts.entries).enumerate() {
        if exits > 0 {
            writeln!(out, "  {side} exit ply={ply}: {exits}")?;
        }
        if entries > 0 {
            writeln!(out, "  {side} cycle-entry ply={ply}: {entries}")?;
        }
    }
    writeln!(out, "cycle components {side}: {}", cycles.components.len())?;
    let mut distance = vec![u8::MAX; graph.len()];
    let mut queue = Vec::new();
    for (id, component) in cycles.components.iter().enumerate() {
        let edges: usize = component
            .iter()
            .map(|&v| {
                graph
                    .edges(v as usize)
                    .iter()
                    .filter(|e| cycles.component[e.to()] == id as u32)
                    .count()
            })
            .sum();
        let shortest =
            shortest_cycle(graph, &cycles, id, &mut distance, &mut queue, CYCLE_WORK_LIMIT)
                .labels();
        let entry = component
            .iter()
            .map(|&v| v as usize)
            .find(|&v| {
                traversal.parent[v] == NONE
                    || cycles.component[traversal.parent[v] as usize] != id as u32
            })
            .expect("根から到達可能な成分の入口");
        writeln!(
            out,
            "  {side} component={} positions={} edges={edges} shortest-cycle={} within-16={}",
            id + 1,
            component.len(),
            shortest.0,
            shortest.1
        )?;
        writeln!(out, "    entry key={}", graph.key(entry)?)?;
        writeln!(out, "    startpos moves {}", traversal.path(graph, entry))?;
    }
    Ok(())
}

fn open(path: &Path) -> Result<BufReader<File>> {
    Ok(BufReader::new(
        File::open(path).with_context(|| format!("読めません: {}", path.display()))?,
    ))
}

pub(super) fn run(args: CheckArgs) -> Result<()> {
    run_to(args, &mut std::io::BufWriter::new(std::io::stdout().lock()))
}

fn run_to(args: CheckArgs, out: &mut impl Write) -> Result<()> {
    ensure!(args.bad_eval.is_finite(), "bad-eval must be finite");
    let annot: Annotation = serde_json::from_reader(open(&args.annotation)?)?;
    ensure!(annot.book.sha256 == digest(&args.db)?, "annotation book sha256 mismatch");
    validate_db(open(&args.db)?, &annot)?;
    let selections = read_preset(open(&args.preset)?)?;
    let errors = membership(&annot, &selections, out)?;
    let pool = reply_graph::thread_pool(args.threads)?;
    let index = pool.install(|| BookIndex::new(&annot.positions))?;
    let mut counts = Vec::new();
    let mut eval_details = BufWriter::new(tempfile::tempfile()?);
    let mut path_details = BufWriter::new(tempfile::tempfile()?);
    for side in ["b", "w"] {
        let graph = pool.install(|| {
            reply_graph::build(&index, side, HIRATE_KEY, |m, p| {
                select(p, selections.get(index.key(m)))
            })
        })?;
        let traversal = Traversal::new(&graph);
        counts.push(evaluations(
            &graph,
            &index,
            &annot,
            &traversal,
            side,
            args.bad_eval,
            &mut eval_details,
        )?);
        paths(&graph, &traversal, side, &mut path_details)?;
        writeln!(path_details, "illegal own moves {side}: {}", graph.illegal_moves)?;
    }
    let bad = counts.iter().map(|c| c.bad).sum::<usize>();
    writeln!(out, "bad-eval reachable positions: {bad} (one example per position)")?;
    eval_details.flush()?;
    let mut eval_details = eval_details.into_inner()?;
    std::io::Seek::rewind(&mut eval_details)?;
    std::io::copy(&mut eval_details, out)?;
    writeln!(
        out,
        "positions without a value: b={}, w={} (of b={}, w={} reachable)",
        counts[0].unknown, counts[1].unknown, counts[0].reachable, counts[1].reachable
    )?;
    path_details.flush()?;
    let mut path_details = path_details.into_inner()?;
    std::io::Seek::rewind(&mut path_details)?;
    std::io::copy(&mut path_details, out)?;
    out.flush()?;
    ensure!(
        errors == 0 && bad == 0,
        "プリセットの所属エラーまたは bad-eval の局面があります"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::reply_graph::fixture::{ROOT, after};
    use super::*;

    fn count(edges: &[Vec<u32>]) -> Distribution {
        distribution(edges, &Cycles::new(edges)).unwrap()
    }

    #[test]
    fn acyclic_distribution_matches_path_enumeration() {
        fn enumerate(edges: &[Vec<u32>], v: usize, ply: usize, result: &mut Vec<u128>) {
            if edges[v].is_empty() {
                result.resize(result.len().max(ply + 1), 0);
                result[ply] += 1;
            }
            for &w in &edges[v] {
                enumerate(edges, w as usize, ply + 1, result);
            }
        }
        let edges = vec![vec![1, 2, 4], vec![2, 3], vec![3], vec![], vec![]];
        let mut expected = Vec::new();
        enumerate(&edges, 0, 0, &mut expected);
        let result = count(&edges);
        assert_eq!(result.exits, expected);
        assert_eq!(result.total, 4);
        assert_eq!(result.cycle_entries, 0);
    }

    #[test]
    fn self_loop_including_root_stops_on_entry() {
        let edges = vec![vec![1], vec![1]];
        assert_eq!(count(&edges).entries, [0, 1]);
        assert_eq!(count(&[vec![0]]).entries, [1]);
        let cycles = Cycles::new(edges.as_slice());
        assert_eq!(
            shortest_cycle(
                edges.as_slice(),
                &cycles,
                0,
                &mut [u8::MAX; 2],
                &mut Vec::new(),
                CYCLE_WORK_LIMIT
            ),
            ShortestCycle::Found(1)
        );
    }

    #[test]
    fn two_components_and_merged_prefixes_keep_multiplicity() {
        let edges = vec![
            vec![1, 2, 6],
            vec![3],
            vec![3],
            vec![4],
            vec![5],
            vec![4, 6],
            vec![6],
        ];
        let cycles = Cycles::new(edges.as_slice());
        assert_eq!(cycles.components, [vec![4, 5], vec![6]]);
        let result = count(&edges);
        assert_eq!(result.entries, [0, 1, 0, 2]);
        assert_eq!(result.total, 3);
        assert_eq!(
            shortest_cycle(
                edges.as_slice(),
                &cycles,
                0,
                &mut [u8::MAX; 7],
                &mut Vec::new(),
                CYCLE_WORK_LIMIT
            ),
            ShortestCycle::Found(2)
        );
    }

    #[test]
    fn shortest_cycle_checks_all_vertices_and_sixteen_boundary() {
        for length in [16, 17, 20, 100] {
            let edges: Vec<_> = (0..length).map(|v| vec![((v + 1) % length) as u32]).collect();
            let cycles = Cycles::new(edges.as_slice());
            assert_eq!(
                shortest_cycle(
                    edges.as_slice(),
                    &cycles,
                    0,
                    &mut vec![u8::MAX; length],
                    &mut Vec::new(),
                    CYCLE_WORK_LIMIT
                ),
                if length <= 16 {
                    ShortestCycle::Found(length as u8)
                } else {
                    ShortestCycle::Longer
                }
            );
        }
        let edges = vec![vec![1], vec![2], vec![0, 2]];
        assert_eq!(
            shortest_cycle(
                edges.as_slice(),
                &Cycles::new(edges.as_slice()),
                0,
                &mut [u8::MAX; 3],
                &mut Vec::new(),
                CYCLE_WORK_LIMIT
            ),
            ShortestCycle::Found(1)
        );
    }

    #[test]
    fn counts_exceed_u64_and_overflow_is_reported() {
        let graph = |depth: usize| {
            let mut edges: Vec<_> = (0..depth).map(|v| vec![(v + 1) as u32; 2]).collect();
            edges.push(vec![]);
            edges
        };
        assert_eq!(count(&graph(80)).total, 1u128 << 80);
        let edges = graph(128);
        assert!(distribution(edges.as_slice(), &Cycles::new(edges.as_slice())).is_err());
    }

    #[test]
    fn short_cycle_in_long_component_and_budget_exhaustion() {
        let mut edges: Vec<_> = (0..100).map(|v| vec![(v + 1) % 100]).collect();
        edges[90].push(88);
        let cycles = Cycles::new(edges.as_slice());
        let mut distance = vec![u8::MAX; 100];
        let mut queue = Vec::new();
        assert_eq!(
            shortest_cycle(
                edges.as_slice(),
                &cycles,
                0,
                &mut distance,
                &mut queue,
                CYCLE_WORK_LIMIT
            ),
            ShortestCycle::Found(3)
        );
        assert_eq!(
            shortest_cycle(edges.as_slice(), &cycles, 0, &mut distance, &mut queue, 10),
            ShortestCycle::Unknown
        );
        assert!(distance.iter().all(|&d| d == u8::MAX));
        assert_eq!(ShortestCycle::Unknown.labels(), ("unknown".into(), "unknown"));
        assert_eq!(ShortestCycle::Longer.labels(), (">16".into(), "false"));
        edges[2].push(0);
        assert_eq!(
            shortest_cycle(edges.as_slice(), &cycles, 0, &mut distance, &mut queue, 10),
            ShortestCycle::AtMost(3)
        );
        assert!(distance.iter().all(|&d| d == u8::MAX));
        assert_eq!(ShortestCycle::AtMost(3).labels(), ("<=3".into(), "true"));
    }

    #[test]
    fn cycle_to_acyclic_tail_does_not_reach_exit() {
        let edges = [vec![1], vec![2], vec![1, 3], vec![4], vec![]];
        let counts = count(&edges);
        assert_eq!(counts.entries, [0, 1]);
        assert_eq!(counts.exits, [0, 0]);
        assert_eq!(counts.total, 1);
    }

    #[test]
    fn total_overflows_with_representable_individual_counts() {
        let mut edges: Vec<_> = (0..127).map(|v| vec![v + 1; 2]).collect();
        edges.extend([vec![128, 129], vec![], vec![130], vec![]]);
        assert!(distribution(edges.as_slice(), &Cycles::new(edges.as_slice())).is_err());
        edges[127].pop();
        assert_eq!(count(&edges).total, 1u128 << 127);
    }

    #[test]
    fn six_significant_digits_and_exponent_boundaries() {
        for (value, expected) in [
            (-150.333333333, "-150.333"),
            (0.0, "0"),
            (-0.0, "-0"),
            (999999.9, "1e+06"),
            (0.00001, "1e-05"),
            (0.0001, "0.0001"),
            (12.0, "12"),
            (1.23456789e100, "1.23457e+100"),
        ] {
            assert_eq!(general(value), expected);
        }
    }

    #[test]
    fn run_reports_headers_before_details_and_uses_preset_moves() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("book.db");
        let annotation = dir.path().join("book.annot.json");
        let preset = dir.path().join("book.explore.txt");
        let child = after(HIRATE_KEY, &["2g2f", "3c3d"]);
        std::fs::write(&db, format!("#YANEURAOU-DB2016 1.00\nsfen {HIRATE_KEY} 1\n7g7f none 0 1 1\n2g2f none 0 1 1\n9i9a none 0 1 1\nsfen {child} 3\n2f2e none 0 1 1\n")).unwrap();
        let annot = serde_json::json!({"book":{"sha256":digest(&db).unwrap()},"positions":{
            HIRATE_KEY:{"moves":{
                "7g7f":{"role":"main","evals":[0]},
                "2g2f":{"role":"record","evals":[]},
                "9i9a":{"role":"record","evals":[]}}},
            child:{"moves":{"2f2e":{"role":"main","evals":[-150,-150,-151]}}}
        }});
        std::fs::write(&annotation, serde_json::to_vec(&annot).unwrap()).unwrap();
        let args = |threshold| CheckArgs {
            db: db.clone(),
            annotation: annotation.clone(),
            preset: preset.clone(),
            bad_eval: threshold,
            threads: 1,
        };
        std::fs::write(&preset, "").unwrap();
        let mut output = Vec::new();
        assert!(run_to(args(-150.0), &mut output).is_ok());
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("bad-eval reachable positions: 0"));
        assert!(text.contains("b exit ply=1: 1"));
        std::fs::write(&preset, format!("sfen {HIRATE_KEY} 2g2f 9i9a\n")).unwrap();
        let mut output = Vec::new();
        assert!(run_to(args(-150.0), &mut output).is_err());
        let text = String::from_utf8(output).unwrap();
        let labels = [
            "preset membership errors: 0",
            "preset record selections: 2",
            "  record selection:",
            "bad-eval reachable positions: 1",
            "  side=b eval=-150.333",
            "positions without a value:",
            "paths b:",
            "b exit ply=3: 1",
            "cycle components b:",
            "illegal own moves b: 1",
            "paths w:",
            "cycle components w:",
            "illegal own moves w: 0",
        ];
        let mut rest = text.as_str();
        for label in labels {
            let at = rest.find(label).unwrap_or_else(|| panic!("missing {label}: {text}"));
            rest = &rest[at + label.len()..];
        }
        assert!(run_to(args(-200.0), &mut Vec::new()).is_ok());
        std::fs::write(&preset, format!("sfen {HIRATE_KEY} unknown 2g2f\n")).unwrap();
        let mut output = Vec::new();
        assert!(run_to(args(-200.0), &mut output).is_err());
        let text = String::from_utf8(output).unwrap();
        assert!(
            text.find("preset membership errors: 1").unwrap()
                < text.find("  unknown move:").unwrap()
        );
        assert!(
            text.find("  unknown move:").unwrap() < text.find("preset record selections:").unwrap()
        );
    }

    fn annotation() -> Annotation {
        serde_json::from_value(serde_json::json!({
            "book": {"sha256": "test"},
            "positions": {ROOT: {"moves": {
                "5i4h": {"role": "main", "evals": [-150, -151]},
                "5i6h": {"role": "record", "evals": []},
                "5i5h": {"role": "alt", "evals": []}
            }}}
        }))
        .unwrap()
    }

    #[test]
    fn read_main_then_mean_then_top_without_rounding() {
        let mut annot = annotation();
        let p = annot.positions.get_mut(ROOT).unwrap();
        assert_eq!(p.value(), (Some(-150.5), "evals"));
        p.read = Some(Reading {
            main: Some(-149.5),
            top: Some(-200.0),
        });
        assert_eq!(p.value(), (Some(-149.5), "main"));
        p.read.as_mut().unwrap().main = None;
        assert_eq!(p.value(), (Some(-150.5), "evals"));
        p.moves.get_mut("5i4h").unwrap().evals.clear();
        assert_eq!(p.value(), (Some(-200.0), "top"));
        p.read = None;
        assert_eq!(p.value(), (None, "top"));
    }

    #[test]
    fn membership_records_and_explicit_selections() {
        let annot = annotation();
        let text = format!(
            "# preset\nsfen {ROOT} 5i6h unknown\nsfen {ROOT} 5i6h\nsfen missing b - 7g7f\n"
        );
        let selections = read_preset(text.as_bytes()).unwrap();
        let mut out = Vec::new();
        assert_eq!(membership(&annot, &selections, &mut out).unwrap(), 2);
        let output = String::from_utf8(out).unwrap();
        assert!(output.contains("preset record selections: 1 positions=1"));
        assert!(output.contains("unknown position: missing b -"));
        assert!(output.contains(&format!("unknown move: {ROOT} | unknown")));
        let p = &annot.positions[ROOT];
        assert_eq!(
            select(p, selections.get(ROOT))
                .iter()
                .map(|&i| p.move_text(i))
                .collect::<Vec<_>>(),
            ["5i6h"]
        );
        assert_eq!(select(p, None).iter().map(|&i| p.move_text(i)).collect::<Vec<_>>(), ["5i4h"]);
        let records_only = read_preset(format!("sfen {ROOT} 5i6h").as_bytes()).unwrap();
        assert_eq!(membership(&annot, &records_only, &mut Vec::new()).unwrap(), 0);
        assert!(read_preset(b"sfen broken\n".as_slice()).is_err());
    }

    #[test]
    fn db_sets_duplicates_and_empty_positions_are_checked() {
        let annot = annotation();
        let valid = format!(
            "#YANEURAOU-DB2016 1.00\nsfen {ROOT} 1\n5i4h none 100 1 1\n5i5h none 100 1 1\n5i6h none -30000 0 0\n"
        );
        assert!(validate_db(valid.as_bytes(), &annot).is_ok());
        assert!(validate_db(format!("{valid}5i6h none 0 0 0\n").as_bytes(), &annot).is_err());
        assert!(validate_db(format!("{valid}sfen {ROOT} 2\n").as_bytes(), &annot).is_err());
        assert!(validate_db(valid.replace("5i6h", "5i6i").as_bytes(), &annot).is_err());
        assert!(validate_db(format!("sfen {ROOT} 1\n").as_bytes(), &annot).is_err());
        assert!(validate_db(b"".as_slice(), &annot).is_err());
    }

    #[test]
    fn bad_eval_counts_include_cycle_interior_and_unknown_positions() {
        let mut annot = annotation();
        let c = after(ROOT, &["5i4h", "5a5b"]);
        annot.positions.insert(
            c.clone(),
            AnnotationPosition {
                moves: BTreeMap::from([(
                    "4h5i".into(),
                    AnnotationMove {
                        role: "main".into(),
                        evals: vec![],
                    },
                )]),
                read: None,
            },
        );
        let index = BookIndex::new(&annot.positions).unwrap();
        let graph = reply_graph::build(&index, "b", ROOT, |_, p| select(p, None)).unwrap();
        let traversal = Traversal::new(&graph);
        let mut out = Vec::new();
        let counts =
            evaluations(&graph, &index, &annot, &traversal, "b", -150.0, &mut out).unwrap();
        assert_eq!((counts.bad, counts.unknown, counts.reachable), (1, 1, 2));
        let v = (0..graph.len()).find(|&v| graph.key(v).unwrap() == c).unwrap();
        assert_eq!(traversal.path(&graph, v), "5i4h 5a5b");
        assert_eq!(graph.cycles(|_, _| true).len(), 1);
        assert_eq!(
            evaluations(&graph, &index, &annot, &traversal, "b", -151.0, &mut Vec::new())
                .unwrap()
                .bad,
            0
        );
    }

    #[test]
    fn sha_mismatch_rejected_before_graph_construction() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("book.db");
        let annotation = dir.path().join("book.annot.json");
        std::fs::write(&db, "").unwrap();
        std::fs::write(&annotation, r#"{"book":{"sha256":"wrong"},"positions":{}}"#).unwrap();
        let result = run(CheckArgs {
            db,
            annotation,
            preset: dir.path().join("absent"),
            bad_eval: -150.0,
            threads: 1,
        });
        assert!(result.unwrap_err().to_string().contains("sha256 mismatch"));
    }
}
