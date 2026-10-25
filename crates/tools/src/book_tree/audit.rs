//! 定跡の元データの読みの値を `book_backprop` の後退解析で伝播し、main が伝播後の 1 位から
//! どれだけ負けるかを点検する。元データは書き換えない。

use super::register::{COLUMNS, ROLES};
use crate::book_backprop::{
    BackpropOptions, DrawValues, Graph, MergeMode, PropagationStats, delta_buckets, percentile,
    propagate_values, read_graph, write_backprop_book,
};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub(super) struct AuditArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 点検の表 (TSV) の出力先。main が 1 位から --line 以上負ける局面を書く
    #[arg(long)]
    table: PathBuf,
    /// 点検の線。main が伝播後の 1 位からこの値以上負ける局面を表に出す
    #[arg(long, default_value_t = 40)]
    line: i32,
    /// 先手を自分として解くときの千日手の値 (対局の DrawValueBlack に揃える)
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    draw_value_black: i32,
    /// 後手を自分として解くときの千日手の値 (対局の DrawValueWhite に揃える)
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    draw_value_white: i32,
    /// 値の分かる手だけを載せた、伝播後の値つき .db の出力先
    #[arg(long)]
    out_db: Option<PathBuf>,
}

/// 局面の main・alt の手。record は指さないので持たない。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct BookMove {
    pub mv: String,
    /// 読みの値の平均を丸めた値 (手番の側から)。読みが無ければ `None`。
    pub eval: Option<i32>,
}

/// 元データの局面。`moves[0]` が main。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct MasterPosition {
    pub ply: String,
    pub source: String,
    pub moves: Vec<BookMove>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    /// 1 回の読みの値 (book 内に子局面が無い)。
    Eval,
    /// book 内の子局面から伝播した値。
    Propagated,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Origin::Eval => "eval",
            Origin::Propagated => "propagated",
        }
    }
}

/// 伝播後の手。値の分かる手だけ。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct AuditedMove {
    pub mv: String,
    pub value: i32,
    pub origin: Origin,
    pub eval: Option<i32>,
}

/// 点検の表の 1 行。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Finding {
    pub key: String,
    pub ply: String,
    pub source: String,
    pub main: AuditedMove,
    pub best: AuditedMove,
}

impl Finding {
    pub fn gap(&self) -> i32 {
        self.best.value - self.main.value
    }
}

/// 手番ごと (0 = 先手番、1 = 後手番) の数。
type BySide = [usize; 2];

#[derive(Debug, Default, PartialEq)]
pub(super) struct AuditSummary {
    pub positions: BySide,
    /// main・alt の手の数。
    pub moves: usize,
    pub records: usize,
    /// 値の分かる手の数。
    pub known_moves: usize,
    pub illegal_moves: usize,
    /// main・alt が 2 手以上の局面の数。
    pub multi_move_positions: BySide,
    /// 読みの値のある手のうち、伝播で値が変わった手の、差の絶対値ごとの数。
    pub abs_deltas: BTreeMap<i32, usize>,
    /// 読みの値が無く、伝播で値が付いた手の数。
    pub propagated_without_eval: usize,
    /// main が 1 位から線未満で負ける局面の数。
    pub below_line: BySide,
    /// main が 1 位から線以上負ける局面の数。
    pub at_or_above_line: BySide,
    pub unknown_main: BySide,
    /// main の値が分からない局面のうち、main・alt が 2 手以上の局面の数。
    pub unknown_main_multi: BySide,
    pub nontrivial_sccs: usize,
    pub max_scc_size: usize,
}

pub(super) fn run(args: AuditArgs) -> Result<()> {
    ensure!(args.line > 0, "--line は 1 以上にしてください");
    for out in std::iter::once(&args.table).chain(args.out_db.as_ref()) {
        ensure_not_same_file(&args.master, out)?;
    }
    let positions = read_master(&args.master)?;
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let work = tempfile::tempdir().context("一時ディレクトリを作れません")?;
    let (findings, summary, graph) = audit(&positions, draws, args.line, work.path())?;
    write_atomic(&args.table, &format_table(&findings))?;
    if let Some(out) = &args.out_db {
        write_backprop_book(&graph, out)?;
    }
    print!("{}", format_summary(&summary, args.line, draws));
    Ok(())
}

/// 出力先が元データと同じファイルなら止める (点検は元データを書き換えない)。
fn ensure_not_same_file(master: &Path, out: &Path) -> Result<()> {
    let Ok(out) = out.canonicalize() else {
        return Ok(());
    };
    let master = master
        .canonicalize()
        .with_context(|| format!("{} を開けません", master.display()))?;
    ensure!(out != master, "出力先が元データと同じです: {}", out.display());
    Ok(())
}

/// 元データを読み、局面ごとに main・alt の手 (main が先頭) を集める。record は数だけ返す。
pub(super) fn read_master(path: &Path) -> Result<(BTreeMap<String, MasterPosition>, usize)> {
    let file =
        std::fs::File::open(path).with_context(|| format!("{} を読めません", path.display()))?;
    parse_master_rows(BufReader::new(file))
}

pub(super) fn parse_master_rows(
    reader: impl BufRead,
) -> Result<(BTreeMap<String, MasterPosition>, usize)> {
    let col = |name: &str| COLUMNS.iter().position(|c| *c == name).expect("既知の列");
    let (key_col, ply_col, move_col, role_col, source_col, evals_col) =
        (col("key"), col("ply"), col("move"), col("role"), col("source"), col("evals"));
    let mut lines = reader.lines();
    let header = lines.next().transpose()?;
    ensure!(
        header.as_deref() == Some(COLUMNS.join("\t").as_str()),
        "元データの見出しが違います"
    );
    let mut positions: BTreeMap<String, MasterPosition> = BTreeMap::new();
    let mut records = 0;
    for (i, line) in lines.enumerate() {
        let line = line?;
        let at = i + 2;
        let cells: Vec<&str> = line.split('\t').collect();
        ensure!(cells.len() == COLUMNS.len(), "元データの {at} 行目の列の数が違います");
        let role = cells[role_col];
        ensure!(ROLES.contains(&role), "元データの {at} 行目の role が不正です");
        if role == "record" {
            records += 1;
            continue;
        }
        let eval = mean_eval(cells[evals_col])
            .with_context(|| format!("元データの {at} 行目の evals が不正です"))?;
        let position =
            positions.entry(cells[key_col].to_owned()).or_insert_with(|| MasterPosition {
                ply: String::new(),
                source: String::new(),
                moves: Vec::new(),
            });
        ensure!(
            position.moves.iter().all(|m| m.mv != cells[move_col]),
            "元データの {at} 行目の手が同じ局面で重なっています"
        );
        let book_move = BookMove {
            mv: cells[move_col].to_owned(),
            eval,
        };
        if role == "main" {
            ensure!(position.ply.is_empty(), "元データの {at} 行目: 局面に main が 2 つあります");
            position.ply = cells[ply_col].to_owned();
            position.source = cells[source_col].to_owned();
            position.moves.insert(0, book_move);
        } else {
            position.moves.push(book_move);
        }
    }
    if let Some((key, _)) = positions.iter().find(|(_, p)| p.ply.is_empty()) {
        bail!("main の無い局面があります: {key}");
    }
    Ok((positions, records))
}

/// カンマ区切りの読みの値の平均を、0.5 は偶数の側へ丸める (Python の `round` と同じ)。空なら `None`。
fn mean_eval(text: &str) -> Result<Option<i32>> {
    if text.is_empty() {
        return Ok(None);
    }
    let values: Vec<i64> = text.split(',').map(str::parse).collect::<Result<_, _>>()?;
    let mean = values.iter().sum::<i64>() as f64 / values.len() as f64;
    Ok(Some(i32::try_from(mean.round_ties_even() as i64)?))
}

/// 局面の手を `.db` に書く。`known` が `Some` なら値の分かる手だけを書き、手の無い局面は書かない。
fn write_value_db(
    path: &Path,
    positions: &BTreeMap<String, MasterPosition>,
    known: Option<&[Vec<bool>]>,
) -> Result<()> {
    let file =
        std::fs::File::create(path).with_context(|| format!("{} を作れません", path.display()))?;
    let mut writer = BufWriter::new(file);
    writeln!(writer, "{}", crate::book_backprop::BOOK_HEADER)?;
    for (index, (key, position)) in positions.iter().enumerate() {
        let selected: Vec<(usize, &BookMove)> = position
            .moves
            .iter()
            .enumerate()
            .filter(|&(i, _)| known.is_none_or(|known| known[index][i]))
            .collect();
        if selected.is_empty() {
            continue;
        }
        writeln!(writer, "sfen {key} {}", position.ply)?;
        for (i, m) in selected {
            // 値の分かる手だけを書く 2 回目は、読みの値の無い手も子局面からの伝播で値が付くので depth=1 にする。
            let depth = i32::from(m.eval.is_some() || known.is_some());
            let count = if i == 0 { 2 } else { 1 };
            writeln!(writer, "{} none {} {depth} {count}", m.mv, m.eval.unwrap_or(0))?;
        }
    }
    writer.flush()?;
    Ok(())
}

/// 値の分かる手を求める。読みの値のある合法手と、book 内の子局面に値の分かる手がある手。
fn known_moves(
    positions: &BTreeMap<String, MasterPosition>,
    graph: &Graph,
) -> Result<Vec<Vec<bool>>> {
    let node_of: Vec<usize> = positions
        .keys()
        .map(|key| graph.find(key).with_context(|| format!("内部エラー: 局面がありません: {key}")))
        .collect::<Result<_>>()?;
    let mut known: Vec<Vec<bool>> = Vec::with_capacity(positions.len());
    let mut node_known = vec![false; graph.len()];
    // (子局面, 親の局面の番号, 手の番号)。子局面の値が分かったら親の手も分かる。
    let mut waiting: Vec<(usize, usize, usize)> = Vec::new();
    for (index, position) in positions.values().enumerate() {
        let node = node_of[index];
        let moves: Vec<_> = graph.moves(node).collect();
        ensure!(moves.len() == position.moves.len(), "内部エラー: 手の数が合いません");
        let row: Vec<bool> = moves
            .iter()
            .zip(&position.moves)
            .map(|(m, b)| m.usable && b.eval.is_some())
            .collect();
        for (i, m) in moves.iter().enumerate() {
            if let (false, true, Some(edge)) = (row[i], m.usable, m.edge) {
                waiting.push((edge.to, index, i));
            }
        }
        node_known[node] = row.iter().any(|&k| k);
        known.push(row);
    }
    waiting.sort_unstable();
    let mut stack: Vec<usize> = (0..graph.len()).filter(|&v| node_known[v]).collect();
    while let Some(child) = stack.pop() {
        let start = waiting.partition_point(|&(c, _, _)| c < child);
        for &(_, index, i) in waiting[start..].iter().take_while(|&&(c, _, _)| c == child) {
            if known[index][i] {
                continue;
            }
            known[index][i] = true;
            let parent = node_of[index];
            if !node_known[parent] {
                node_known[parent] = true;
                stack.push(parent);
            }
        }
    }
    Ok(known)
}

/// 元データを伝播して main を点検する。点検の表 (差の大きい順)・要約・伝播後のグラフを返す。
pub(super) fn audit(
    (positions, records): &(BTreeMap<String, MasterPosition>, usize),
    draws: DrawValues,
    line: i32,
    work: &Path,
) -> Result<(Vec<Finding>, AuditSummary, Graph)> {
    let mut summary = AuditSummary {
        records: *records,
        ..AuditSummary::default()
    };
    let all_db = work.join("all.db");
    write_value_db(&all_db, positions, None)?;
    let all = read_graph(&all_db, BackpropOptions::default())?;
    summary.illegal_moves = all.illegal_moves;
    let known = known_moves(positions, &all)?;
    drop(all);

    let known_db = work.join("known.db");
    write_value_db(&known_db, positions, Some(&known))?;
    let options = BackpropOptions {
        skip_unusable_moves: true,
        ..BackpropOptions::default()
    };
    let mut graph = read_graph(&known_db, options)?;
    let stats: PropagationStats = propagate_values(&mut graph, draws, MergeMode::Replace)?;
    summary.nontrivial_sccs = stats.nontrivial_sccs;
    summary.max_scc_size = stats.max_scc_size;

    let mut findings = Vec::new();
    for ((key, position), known) in positions.iter().zip(&known) {
        let side = usize::from(key.split(' ').nth(1) == Some("w"));
        summary.positions[side] += 1;
        summary.moves += position.moves.len();
        if position.moves.len() >= 2 {
            summary.multi_move_positions[side] += 1;
        }
        let audited: Vec<Option<AuditedMove>> = match graph.find(key) {
            Some(node) => {
                let mut values = graph.moves(node);
                position
                    .moves
                    .iter()
                    .zip(known)
                    .map(|(m, &k)| -> Result<_> {
                        if !k {
                            return Ok(None);
                        }
                        let value = values.next().context("内部エラー: 手の数が合いません")?;
                        let origin = if value.edge.is_some() {
                            Origin::Propagated
                        } else {
                            ensure!(m.eval.is_some(), "内部エラー: 値の無い手です: {key} {}", m.mv);
                            Origin::Eval
                        };
                        Ok(Some(AuditedMove {
                            mv: m.mv.clone(),
                            value: value.new,
                            origin,
                            eval: m.eval,
                        }))
                    })
                    .collect::<Result<_>>()?
            }
            None => vec![None; position.moves.len()],
        };
        for m in audited.iter().flatten() {
            summary.known_moves += 1;
            match m.eval {
                Some(eval) if eval != m.value => {
                    *summary.abs_deltas.entry((m.value - eval).abs()).or_default() += 1;
                }
                None => summary.propagated_without_eval += 1,
                _ => {}
            }
        }
        let Some(main) = &audited[0] else {
            summary.unknown_main[side] += 1;
            if position.moves.len() >= 2 {
                summary.unknown_main_multi[side] += 1;
            }
            continue;
        };
        // 同じ値なら main を 1 位とみなす (main が先頭なので最初の最大値)。
        let best = audited
            .iter()
            .flatten()
            .reduce(|best, m| if m.value > best.value { m } else { best })
            .expect("main がある");
        let gap = best.value - main.value;
        if gap == 0 {
            continue;
        }
        if gap < line {
            summary.below_line[side] += 1;
        } else {
            summary.at_or_above_line[side] += 1;
            findings.push(Finding {
                key: key.clone(),
                ply: position.ply.clone(),
                source: position.source.clone(),
                main: main.clone(),
                best: best.clone(),
            });
        }
    }
    findings.sort_by(|a, b| b.gap().cmp(&a.gap()).then_with(|| a.key.cmp(&b.key)));
    Ok((findings, summary, graph))
}

pub(super) fn format_table(findings: &[Finding]) -> String {
    let mut text = String::from(
        "key\tply\tside\tsource\tmain\tmain_value\tmain_from\tmain_eval\tbest\tbest_value\tbest_from\tbest_eval\tgap\tmain_value_black\tbest_value_black\n",
    );
    for f in findings {
        let black = !f.key.split(' ').nth(1).is_some_and(|s| s == "w");
        let side = if black { "b" } else { "w" };
        let sente = |v: i32| if black { v } else { -v };
        let eval = |m: &AuditedMove| m.eval.map_or_else(String::new, |v| v.to_string());
        writeln!(
            text,
            "{}\t{}\t{side}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            f.key,
            f.ply,
            f.source,
            f.main.mv,
            f.main.value,
            f.main.origin.as_str(),
            eval(&f.main),
            f.best.mv,
            f.best.value,
            f.best.origin.as_str(),
            eval(&f.best),
            f.gap(),
            sente(f.main.value),
            sente(f.best.value),
        )
        .expect("String への書き込み");
    }
    text
}

pub(super) fn format_summary(summary: &AuditSummary, line: i32, draws: DrawValues) -> String {
    let sides = |c: BySide| format!("{} (先手番 {} / 後手番 {})", c[0] + c[1], c[0], c[1]);
    let changed: usize = summary.abs_deltas.values().sum();
    let mut text = String::new();
    let mut out = |s: String| {
        text.push_str(&s);
        text.push('\n');
    };
    out(format!("千日手の値: 先手 {} / 後手 {}", draws.black, draws.white));
    out(format!("局面: {}", sides(summary.positions)));
    out(format!(
        "手 (main・alt): {} / 値の分かる手 {} / 非合法手 {} / record (除外) {}",
        summary.moves, summary.known_moves, summary.illegal_moves, summary.records
    ));
    if changed > 0 {
        let buckets = delta_buckets(&summary.abs_deltas)
            .into_iter()
            .map(|(label, count)| format!("{label}: {count}"))
            .collect::<Vec<_>>()
            .join(" / ");
        out(format!(
            "読みの値から伝播で変わった手: {changed} (差の中央値 {} / 90% {} / 最大 {}) [{buckets}]",
            percentile(&summary.abs_deltas, changed, 50),
            percentile(&summary.abs_deltas, changed, 90),
            summary.abs_deltas.keys().next_back().copied().unwrap_or(0),
        ));
    } else {
        out("読みの値から伝播で変わった手: 0".into());
    }
    out(format!("読みの値が無く伝播で値が付いた手: {}", summary.propagated_without_eval));
    out(format!("main・alt が 2 手以上の局面: {}", sides(summary.multi_move_positions)));
    out(format!(
        "main が 1 位から 1〜{} 負ける局面: {}",
        line - 1,
        sides(summary.below_line)
    ));
    out(format!(
        "main が 1 位から {line} 以上負ける局面: {}",
        sides(summary.at_or_above_line)
    ));
    out(format!(
        "main の値が分からない局面: {} / うち main・alt が 2 手以上: {}",
        sides(summary.unknown_main),
        sides(summary.unknown_main_multi)
    ));
    out(format!(
        "循環 (自明でない強連結成分): {} (最大 {} 局面)",
        summary.nontrivial_sccs, summary.max_scc_size
    ));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
    /// ROOT から 5i4h。
    const A: &str = "4k4/9/9/9/9/9/9/5K3/9 w -";
    /// A から 5a5b。
    const B: &str = "9/4k4/9/9/9/9/9/5K3/9 b -";

    fn master(rows: &[(&str, &str, &str, &str)]) -> String {
        let mut text = COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, evals) in rows {
            writeln!(text, "{key}\t1\t{mv}\t{role}\tread\t{evals}\t\t\twide\tt\t").unwrap();
        }
        text
    }

    fn run_audit(text: &str, line: i32) -> (Vec<Finding>, AuditSummary) {
        let parsed = parse_master_rows(text.as_bytes()).unwrap();
        let work = tempfile::tempdir().unwrap();
        let (findings, summary, _) =
            audit(&parsed, DrawValues::both(0), line, work.path()).unwrap();
        (findings, summary)
    }

    #[test]
    fn unknown_moves_and_records_do_not_compete() {
        // 値の分からない 5i5h (0) と record (9999) を数えると main が 1 位でなくなる。
        let text = master(&[
            (ROOT, "5i4h", "main", ""),
            (ROOT, "5i6h", "alt", "-60,-40"),
            (ROOT, "5i5h", "alt", ""),
            (A, "5a5b", "main", "30"),
            (A, "5a4b", "record", "9999"),
        ]);
        let (findings, summary) = run_audit(&text, 1);
        assert!(findings.is_empty());
        assert_eq!(summary.records, 1);
        assert_eq!(summary.known_moves, 3);
        assert_eq!(summary.below_line, [0, 0]);
        assert_eq!(summary.unknown_main, [0, 0]);
        assert_eq!(summary.propagated_without_eval, 1);
    }

    #[test]
    fn findings_respect_line_and_report_sente_values() {
        let text = master(&[
            (ROOT, "5i4h", "main", ""),
            (ROOT, "5i6h", "alt", "10"),
            (A, "5a5b", "main", "30"),
            (A, "5a6b", "alt", "80"),
        ]);
        let (findings, summary) = run_audit(&text, 50);
        assert_eq!(summary.at_or_above_line, [1, 1]);
        assert_eq!(findings.len(), 2);
        // 根: main 5i4h は A の 1 位 (80) の符号反転、1 位は 5i6h の読みの値。
        assert_eq!(findings[0].key, ROOT);
        assert_eq!(findings[0].gap(), 90);
        assert_eq!(findings[0].main.origin, Origin::Propagated);
        assert_eq!(findings[0].best.origin, Origin::Eval);
        let table = format_table(&findings);
        let a_row = table.lines().find(|l| l.starts_with(A)).unwrap();
        let cells: Vec<_> = a_row.split('\t').collect();
        assert_eq!(
            &cells[2..],
            [
                "w", "read", "5a5b", "30", "eval", "30", "5a6b", "80", "eval", "80", "50", "-30",
                "-80"
            ]
        );

        let (findings, summary) = run_audit(&text, 51);
        assert_eq!(findings.len(), 1);
        assert_eq!(summary.below_line, [0, 1]);
    }

    #[test]
    fn move_without_eval_is_known_through_grandchild() {
        let text = master(&[
            (ROOT, "5i4h", "main", ""),
            (ROOT, "5i6h", "alt", "10"),
            (A, "5a5b", "main", ""),
            (B, "4h4g", "main", "20"),
        ]);
        let (findings, summary) = run_audit(&text, 5);
        assert_eq!(summary.known_moves, 4);
        assert_eq!(summary.unknown_main, [0, 0]);
        // 根の 5i4h は B の 20 が A で -20、根で 20 になる。1 位なので表に出ない。
        assert!(findings.is_empty());
        assert_eq!(summary.below_line, [0, 0]);
    }

    #[test]
    fn main_without_value_is_counted_and_skipped() {
        let text = master(&[(ROOT, "5i5h", "main", ""), (ROOT, "5i6h", "alt", "10")]);
        let (findings, summary) = run_audit(&text, 1);
        assert!(findings.is_empty());
        assert_eq!(summary.unknown_main, [1, 0]);
        assert_eq!(summary.unknown_main_multi, [1, 0]);
        assert_eq!(summary.known_moves, 1);
    }

    #[test]
    fn mean_eval_rounds_half_to_even() {
        assert_eq!(mean_eval("").unwrap(), None);
        assert_eq!(mean_eval("1,2").unwrap(), Some(2));
        assert_eq!(mean_eval("2,3").unwrap(), Some(2));
        assert_eq!(mean_eval("-1,-2").unwrap(), Some(-2));
        assert!(mean_eval("1,x").is_err());
    }

    #[test]
    fn rejects_broken_masters() {
        let two_mains = master(&[(ROOT, "5i5h", "main", ""), (ROOT, "5i6h", "main", "")]);
        assert!(parse_master_rows(two_mains.as_bytes()).is_err());
        let no_main = master(&[(ROOT, "5i6h", "alt", "10")]);
        assert!(parse_master_rows(no_main.as_bytes()).is_err());
        let duplicate = master(&[(ROOT, "5i6h", "main", ""), (ROOT, "5i6h", "alt", "10")]);
        assert!(parse_master_rows(duplicate.as_bytes()).is_err());
    }
}
