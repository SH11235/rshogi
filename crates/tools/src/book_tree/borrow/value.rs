//! `book_mine borrow value`: 出口と確認点の読みを台帳から引き、部分木を後退解析で根まで伝播して
//! 定跡の元データに登録する。

use super::cut::master_value;
use super::{Kind, Meta, RootInfo, Subtree, read_cut, side, turn};
use crate::book_backprop::{
    BOOK_HEADER, BackpropOptions, DrawValues, MergeMode, propagate_values, read_graph,
};
use crate::book_tree::audit::{MasterValue, audit, master_values, parse_master_rows, read_master};
use crate::book_tree::journal::{Reading, read_source};
use crate::book_tree::register::{
    self, Conflict, NodeMove, Options, format_master, merge, parse_master,
};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

/// 0 行の読みの値。`bestmove resign` は詰み (負け)、勝ち宣言は勝ち。
const MATE: i32 = 30000;

#[derive(Args, Debug)]
pub(super) struct ValueArgs {
    /// `borrow cut` の出力先
    #[arg(long)]
    cut: PathBuf,
    /// 読みの台帳 (tree serve の journal。serve が書いている間は複製を渡す)
    #[arg(long)]
    journal: PathBuf,
    /// 台帳の読み条件の指紋。省略時は台帳内の唯一の条件
    #[arg(long)]
    fingerprint: Option<String>,
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 結果の出力先 (まだ無い dir)
    #[arg(long)]
    out: PathBuf,
    /// 足す行の since 列
    #[arg(long)]
    since: String,
    /// 足す行の note 列の末尾に付ける文
    #[arg(long, default_value = "")]
    note: String,
    /// main からこの値未満の差の手を alt にする
    #[arg(long, default_value_t = 40)]
    alt_gap: i32,
    /// 確認点で、候補の外の手が候補の最善よりこの値以上良ければ印を付ける
    #[arg(long, default_value_t = 40)]
    line: i32,
    /// 先手を自分として解くときの千日手の値 (対局の DrawValueBlack に揃える)
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    draw_value_black: i32,
    /// 後手を自分として解くときの千日手の値 (対局の DrawValueWhite に揃える)
    #[arg(long, default_value_t = 70, allow_hyphen_values = true)]
    draw_value_white: i32,
    /// 元データを書き換える。省くと出力先に結果を書くだけ
    #[arg(long)]
    apply: bool,
}

#[cfg(test)]
impl ValueArgs {
    pub(super) fn for_test(
        cut: PathBuf,
        journal: PathBuf,
        master: PathBuf,
        out: PathBuf,
        apply: bool,
    ) -> Self {
        Self {
            cut,
            journal,
            fingerprint: None,
            master,
            out,
            since: "tag".into(),
            note: String::new(),
            alt_gap: 40,
            line: 40,
            draw_value_black: -70,
            draw_value_white: 70,
            apply,
        }
    }
}

/// 伝播後の手の値 (手番の側から)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MoveValue {
    pub mv: String,
    pub value: i32,
    /// 部分木の手 (大きい定跡の手か --extra の手)。偽なら出口の読みの手。
    pub expanded: bool,
    pub book_value: Option<i32>,
}

/// 確認点の印: 候補の外の手が候補の最善を線以上上回る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Mark {
    pub key: String,
    pub ply: u32,
    pub mv: String,
    pub cp: i32,
    pub best: String,
    pub best_value: i32,
}

/// 根 1 本の結果。`moves` は局面ごとに値の降順。
#[derive(Debug, Default)]
pub(super) struct RootValues {
    pub moves: BTreeMap<String, Vec<MoveValue>>,
    pub marks: Vec<Mark>,
}

/// 部分木の読みの局面の読み。
pub(super) type ReadingsOf = BTreeMap<String, Reading>;

/// `.db` に書いた 1 行。
struct Written<'a> {
    /// 指し手。`None` は指し手の無い値だけの行。
    mv: Option<&'a str>,
    /// 部分木の手か。
    expanded: bool,
    book_value: Option<i32>,
}

fn leaf_value(reading: &Reading) -> i32 {
    if reading.bestmove == "win" {
        reading.multipv.first().map_or(MATE, |p| p.cp)
    } else {
        -MATE
    }
}

/// 部分木を伝播する。出口は読みの手 (元データで止めた出口は元データの値)、途中の局面は部分木の手を持つ。
pub(super) fn value_root(
    root: &RootInfo,
    tree: &Subtree,
    readings: &ReadingsOf,
    master: &BTreeMap<String, MasterValue>,
    draws: DrawValues,
    line: i32,
    work: &Path,
) -> Result<RootValues> {
    let mut edges: BTreeMap<&str, Vec<&super::Edge>> = BTreeMap::new();
    for e in &tree.edges {
        edges.entry(e.key.as_str()).or_default().push(e);
    }
    let mut written: BTreeMap<&str, Vec<Written>> = BTreeMap::new();
    let db = work.join(format!("{}.db", root.id));
    let mut out = BufWriter::new(
        std::fs::File::create(&db).with_context(|| format!("{} を作れません", db.display()))?,
    );
    writeln!(out, "{BOOK_HEADER}")?;
    for (key, node) in &tree.nodes {
        writeln!(out, "sfen {key} {}", node.ply)?;
        let rows = written.entry(key.as_str()).or_default();
        match node.kind {
            Kind::Inner => {
                for e in edges.get(key.as_str()).into_iter().flatten() {
                    writeln!(out, "{} none 0 1 1", e.mv)?;
                    rows.push(Written {
                        mv: Some(&e.mv),
                        expanded: true,
                        book_value: e.book_value,
                    });
                }
            }
            Kind::Master if !node.read => {
                let value = master
                    .get(key)
                    .and_then(|v| master_value(v, turn(key, &root.own) == "own"))
                    .with_context(|| {
                        format!(
                            "{}: 元データで止めた局面の値が元データにありません: {key}",
                            root.id
                        )
                    })?;
                writeln!(out, "none none {value} 1 1")?;
                rows.push(Written {
                    mv: None,
                    expanded: false,
                    book_value: None,
                });
            }
            _ => {
                let reading = readings
                    .get(key)
                    .with_context(|| format!("{}: 読みがありません: {key}", root.id))?;
                if reading.lines().is_empty() {
                    writeln!(out, "none none {} 1 1", leaf_value(reading))?;
                    rows.push(Written {
                        mv: None,
                        expanded: false,
                        book_value: None,
                    });
                }
                for p in reading.lines() {
                    writeln!(out, "{} none {} 1 1", p.mv, p.cp)?;
                    rows.push(Written {
                        mv: Some(&p.mv),
                        expanded: false,
                        book_value: None,
                    });
                }
            }
        }
    }
    out.flush()?;
    drop(out);
    let mut graph = read_graph(&db, BackpropOptions::default())?;
    ensure!(graph.illegal_moves == 0, "{}: 内部エラー: 非合法手があります", root.id);
    propagate_values(&mut graph, draws, MergeMode::Replace)?;

    let mut result = RootValues::default();
    for (key, rows) in written {
        let node = graph
            .find(key)
            .with_context(|| format!("内部エラー: 局面がありません: {key}"))?;
        let values: Vec<i32> = graph.moves(node).map(|m| m.new).collect();
        ensure!(values.len() == rows.len(), "内部エラー: 手の数が合いません: {key}");
        let mut moves: Vec<MoveValue> = rows
            .iter()
            .zip(values)
            .filter_map(|(row, value)| {
                Some(MoveValue {
                    mv: row.mv?.to_owned(),
                    value,
                    expanded: row.expanded,
                    book_value: row.book_value,
                })
            })
            .collect();
        moves.sort_by(|a, b| {
            b.value
                .cmp(&a.value)
                .then_with(|| b.expanded.cmp(&a.expanded))
                .then_with(|| b.book_value.cmp(&a.book_value))
                .then_with(|| a.mv.cmp(&b.mv))
        });
        let n = &tree.nodes[key];
        if n.kind == Kind::Inner && n.read {
            let reading = readings
                .get(key)
                .with_context(|| format!("{}: 読みがありません: {key}", root.id))?;
            if let Some(best) = moves.first() {
                let outside = reading
                    .lines()
                    .iter()
                    .filter(|p| !moves.iter().any(|m| m.mv == p.mv))
                    .max_by_key(|p| p.cp);
                if let Some(p) = outside
                    && i64::from(p.cp) >= i64::from(best.value) + i64::from(line)
                {
                    result.marks.push(Mark {
                        key: key.to_owned(),
                        ply: n.ply,
                        mv: p.mv.clone(),
                        cp: p.cp,
                        best: best.mv.clone(),
                        best_value: best.value,
                    });
                }
            }
        }
        result.moves.insert(key.to_owned(), moves);
    }
    Ok(result)
}

/// 登録する局面: 自分の手番で、元データで止めた出口と印の付いた局面を除く。
fn register_nodes(
    index: usize,
    root: &RootInfo,
    tree: &Subtree,
    values: &RootValues,
    max_plies: usize,
    nodes_limit: u64,
) -> BTreeMap<String, register::Node> {
    let marked: BTreeSet<&str> = values.marks.iter().map(|m| m.key.as_str()).collect();
    let mut nodes = BTreeMap::new();
    for (key, node) in &tree.nodes {
        // 元データの局面は元データの値と手を残す (読み直した出口でも替えない)。
        if side(key) != root.own || node.kind == Kind::Master || marked.contains(key.as_str()) {
            continue;
        }
        let moves = &values.moves[key];
        if moves.is_empty() {
            continue;
        }
        nodes.insert(
            key.clone(),
            register::Node {
                ply: node.ply.to_string(),
                remaining: max_plies.saturating_sub(node.level),
                nodes: nodes_limit,
                moves: moves
                    .iter()
                    .map(|m| NodeMove {
                        mv: m.mv.clone(),
                        value: m.value,
                    })
                    .collect(),
                tree: index,
            },
        );
    }
    nodes
}

fn black(key: &str, value: i32) -> i32 {
    if side(key) == "w" { -value } else { value }
}

pub(super) fn run(args: ValueArgs) -> Result<()> {
    ensure!(!args.since.trim().is_empty(), "--since を指定してください");
    for text in [&args.since, &args.note] {
        ensure!(!text.contains(['\t', '\r', '\n']), "since・note にタブや改行は使えません");
    }
    ensure!(args.alt_gap >= 0, "--alt-gap は 0 以上にしてください");
    ensure!(args.line > 0, "--line は 1 以上にしてください");
    ensure!(
        std::fs::symlink_metadata(&args.out).is_err(),
        "{} は既にあります (--out はまだ無い dir にしてください)",
        args.out.display()
    );
    let (meta, trees) = read_cut(&args.cut)?;
    let wanted: BTreeSet<&str> = trees
        .iter()
        .flat_map(|t| t.nodes.iter().filter(|(_, n)| n.read).map(|(k, _)| k.as_str()))
        .collect();
    let source = read_source(&args.journal, args.fingerprint.as_deref())?;
    let readings: ReadingsOf = source
        .readings
        .into_iter()
        .filter(|r| wanted.contains(r.key.as_str()))
        .map(|r| (r.key.clone(), r))
        .collect();
    let missing: Vec<&str> =
        wanted.iter().copied().filter(|k| !readings.contains_key(*k)).collect();
    if !missing.is_empty() {
        bail!(
            "読みが足りません: {} / {} 局面 (例: {})",
            missing.len(),
            wanted.len(),
            missing.iter().take(3).copied().collect::<Vec<_>>().join(" | ")
        );
    }
    println!(
        "台帳の条件 {} (nodes {} / MultiPV {} / 千日手 {:?}) の読み {} 局面",
        source.settings.fingerprint,
        source.settings.nodes,
        source.settings.multipv,
        source.settings.draw(),
        readings.len()
    );
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let work = tempfile::tempdir().context("一時ディレクトリを作れません")?;
    let master_rows = read_master(&args.master)?;
    let master = master_values(&master_rows, draws, work.path())?;
    let (_, before, _) = audit(&master_rows, draws, args.line, work.path())?;

    let mut all = Vec::new();
    let mut nodes = BTreeMap::new();
    let mut disagreements = 0;
    for (index, (root, tree)) in meta.roots.iter().zip(&trees).enumerate() {
        let values = value_root(root, tree, &readings, &master, draws, args.line, work.path())?;
        disagreements += merge(
            &mut nodes,
            register_nodes(
                index,
                root,
                tree,
                &values,
                meta.params.max_plies,
                source.settings.nodes,
            ),
        );
        all.push(values);
    }
    let text = std::fs::read_to_string(&args.master)
        .with_context(|| format!("{} を読めません", args.master.display()))?;
    let rows = parse_master(&text)?;
    let before_rows = rows.len();
    let lineage = format!("peta:{}", meta.book.name);
    let options = Options {
        since: &args.since,
        note: &args.note,
        alt_gap: args.alt_gap,
        source: "peta",
        lineage: &lineage,
    };
    let (rows, conflicts, summary) = register::register(rows, &nodes, &options)?;
    let registered = format_master(&rows);
    // 登録後の元データが点検を通ることを、書き換える前に確かめる。
    let (_, after, _) =
        audit(&parse_master_rows(registered.as_bytes())?, draws, args.line, work.path())?;

    std::fs::create_dir_all(&args.out)?;
    write_outputs(&args.out, &meta, &trees, &all, &conflicts)?;
    for s in ["b", "w"] {
        println!(
            "{s}: main を足した局面 {} / alt を足した手 {}",
            summary.added_main.get(s).unwrap_or(&0),
            summary.added_alt.get(s).unwrap_or(&0)
        );
    }
    println!(
        "main を伝播後の 1 位に替えた局面 {} / 評価値を伝播後の値にした行 {} / record なので足さなかった手 {} / 食い違い {} / 根をまたいで 1 位が違った局面 {disagreements} / 印 {}",
        summary.replaced_main,
        summary.evals_updated,
        summary.record_skipped,
        summary.conflicts,
        all.iter().map(|v| v.marks.len()).sum::<usize>()
    );
    let line_count =
        |s: &crate::book_tree::audit::AuditSummary| s.at_or_above_line[0] + s.at_or_above_line[1];
    println!(
        "点検 (main が 1 位から {} 以上負ける局面): 前 {} → 後 {}",
        args.line,
        line_count(&before),
        line_count(&after)
    );
    if args.apply {
        write_atomic(&args.master, &registered)?;
        println!("書き換えました: 行 {before_rows} -> {}", rows.len());
    } else {
        println!("確認だけ (--apply なし): 行 {before_rows} -> {}", rows.len());
    }
    Ok(())
}

fn write_outputs(
    out: &Path,
    meta: &Meta,
    trees: &[Subtree],
    all: &[RootValues],
    conflicts: &[Conflict],
) -> Result<()> {
    let mut values = BufWriter::new(std::fs::File::create(out.join("values.tsv"))?);
    writeln!(
        values,
        "id\tkey\tply\tlevel\tside\tturn\tkind\tmove\tvalue\tvalue_black\texpanded\tbook_value"
    )?;
    let mut roots = String::from(
        "id\town\tkey\tply\tkind\tbest\tbest_value\tbest_value_black\tsecond\tsecond_value\tmarks\n",
    );
    let mut marks = String::from("id\tkey\tply\tside\tmove\tcp\tbest\tbest_value\tgap\n");
    for ((root, tree), result) in meta.roots.iter().zip(trees).zip(all) {
        for (key, node) in &tree.nodes {
            for m in &result.moves[key] {
                writeln!(
                    values,
                    "{}\t{key}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    root.id,
                    node.ply,
                    node.level,
                    side(key),
                    turn(key, &root.own),
                    node.kind.as_str(),
                    m.mv,
                    m.value,
                    black(key, m.value),
                    u8::from(m.expanded),
                    m.book_value.map_or_else(String::new, |v| v.to_string())
                )?;
            }
        }
        let moves = &result.moves[&root.key];
        let cell = |i: usize| -> (String, String) {
            moves
                .get(i)
                .map_or((String::new(), String::new()), |m| (m.mv.clone(), m.value.to_string()))
        };
        let (best, best_value) = cell(0);
        let (second, second_value) = cell(1);
        let best_black = moves
            .first()
            .map_or_else(String::new, |m| black(&root.key, m.value).to_string());
        writeln!(
            roots,
            "{}\t{}\t{}\t{}\t{}\t{best}\t{best_value}\t{best_black}\t{second}\t{second_value}\t{}",
            root.id,
            root.own,
            root.key,
            root.ply,
            tree.nodes[&root.key].kind.as_str(),
            result.marks.len()
        )?;
        for m in &result.marks {
            writeln!(
                marks,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                root.id,
                m.key,
                m.ply,
                side(&m.key),
                m.mv,
                m.cp,
                m.best,
                m.best_value,
                m.cp - m.best_value
            )?;
        }
    }
    values.flush()?;
    write_atomic(&out.join("roots.tsv"), &roots)?;
    write_atomic(&out.join("marks.tsv"), &marks)?;
    let mut table = String::from(
        "key\tply\tside\tmain\tmain_source\tmain_tier\tmain_tree_value\ttree_best\ttree_value\troot\n",
    );
    for c in conflicts {
        writeln!(table, "{c}\t{}", meta.roots[c.tree].id)?;
    }
    write_atomic(&out.join("conflicts.tsv"), &table)?;
    Ok(())
}
