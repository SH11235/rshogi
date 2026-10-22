//! 木の出力 (node_moves.tsv) から定跡の元データの表に手を登録する。

use super::journal::LockedFile;
use super::output::NODE_MOVES;
use crate::common::io::write_atomic;
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

const COLUMNS: [&str; 11] = [
    "key", "ply", "move", "role", "source", "evals", "nodes", "lineage", "tier", "since", "note",
];
const ROLES: [&str; 3] = ["main", "alt", "record"];
/// 読みから作った行。木の結果で main を替え、評価値を上書きしてよい。
const READ_SOURCES: [&str; 2] = ["read", "deep"];

#[derive(Args, Debug)]
pub(super) struct RegisterArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 足す行の since 列
    #[arg(long)]
    since: String,
    /// 足す行の note 列の末尾に付ける文
    #[arg(long, default_value = "")]
    note: String,
    /// main からこの値未満の差の手を alt にする
    #[arg(long, default_value_t = 40)]
    alt_gap: i32,
    /// main を残して木の 1 位と食い違った局面の一覧 (TSV)
    #[arg(long)]
    conflicts: PathBuf,
    /// 元データを書き換える。省くと集計と食い違いの一覧だけを出す
    #[arg(long)]
    apply: bool,
    /// ready の木の出力先。同じ局面は、その局面から先に伸ばせた段数 (remaining) の大きい木を使う
    #[arg(required = true)]
    trees: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub(super) struct NodeMove {
    pub mv: String,
    pub value: i32,
}

#[derive(Debug, Clone)]
pub(super) struct Node {
    pub ply: String,
    pub remaining: usize,
    pub nodes: u64,
    /// 伝播後の値の順。
    pub moves: Vec<NodeMove>,
    pub tree: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Row(pub Vec<String>);
impl Row {
    fn get(&self, column: &str) -> &str {
        &self.0[index(column)]
    }
    fn set(&mut self, column: &str, value: String) {
        self.0[index(column)] = value;
    }
}
fn index(column: &str) -> usize {
    COLUMNS.iter().position(|c| *c == column).expect("既知の列")
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Summary {
    pub added_main: BTreeMap<String, usize>,
    pub added_alt: BTreeMap<String, usize>,
    pub replaced_main: usize,
    pub evals_updated: usize,
    pub record_skipped: usize,
    pub conflicts: usize,
}

pub(super) struct Options<'a> {
    pub since: &'a str,
    pub note: &'a str,
    pub alt_gap: i32,
}

pub(super) fn run(args: RegisterArgs) -> Result<()> {
    ensure!(!args.since.trim().is_empty(), "--since を指定してください");
    for text in [&args.since, &args.note] {
        ensure!(!text.contains(['\t', '\r', '\n']), "since・note にタブや改行は使えません");
    }
    ensure!(args.alt_gap >= 0, "--alt-gap は 0 以上にしてください");
    check_conflicts_path(&args.conflicts, &args.master, &args.trees)?;
    let mut nodes = BTreeMap::new();
    let mut disagreements = 0;
    for (tree, dir) in args.trees.iter().enumerate() {
        // 打ち間違えた出力先に lock のために dir を作らない。
        ensure!(dir.is_dir(), "{}: 木の出力先がありません", dir.display());
        // replay が出力を書き換えている途中の材料を読まない。
        let _lock = LockedFile::open(&dir.join("tree.lock"))?;
        let status = std::fs::read_to_string(dir.join("STATUS"))
            .with_context(|| format!("{}: STATUS を読めません", dir.display()))?;
        ensure!(
            status == "DONE\n",
            "{}: 終わった ready の木ではありません ({})",
            dir.display(),
            status.trim()
        );
        let path = dir.join(NODE_MOVES);
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!("{} を読めません (古い木は tree replay で作り直してください)", path.display())
        })?;
        disagreements += merge(&mut nodes, parse_node_moves(&text, tree)?);
    }
    let text = std::fs::read_to_string(&args.master)
        .with_context(|| format!("{} を読めません", args.master.display()))?;
    let rows = parse_master(&text)?;
    let before = rows.len();
    let options = Options {
        since: &args.since,
        note: &args.note,
        alt_gap: args.alt_gap,
    };
    let (rows, conflicts, summary) = register(rows, &nodes, &options)?;
    let mut table = String::from(
        "key\tply\tside\tmain\tmain_source\tmain_tier\tmain_tree_value\ttree_best\ttree_value\ttree\n",
    );
    for c in &conflicts {
        writeln!(table, "{c}\t{}", args.trees[c.tree].display())?;
    }
    write_atomic(&args.conflicts, &table)?;
    for side in ["b", "w"] {
        println!(
            "{side}: main を足した局面 {} / alt を足した手 {}",
            summary.added_main.get(side).unwrap_or(&0),
            summary.added_alt.get(side).unwrap_or(&0)
        );
    }
    println!(
        "main を木の 1 位に替えた局面 {} / 評価値を伝播後の値にした行 {} / record なので足さなかった手 {} / 食い違い {} ({}) / 木どうしで 1 位が違った局面 {disagreements}",
        summary.replaced_main,
        summary.evals_updated,
        summary.record_skipped,
        summary.conflicts,
        args.conflicts.display()
    );
    if args.apply {
        write_atomic(&args.master, &format_master(&rows))?;
        println!("書き換えました: 行 {before} -> {}", rows.len());
    } else {
        println!("確認だけ (--apply なし): 行 {before} -> {}", rows.len());
    }
    Ok(())
}

/// 食い違いの一覧は確認だけの実行でも書くので、元データや木の出力を指す書き方 (相対・絶対・`..`・リンク) を
/// 実体のパスで比べて拒否する。
pub(super) fn check_conflicts_path(
    conflicts: &Path,
    master: &Path,
    trees: &[PathBuf],
) -> Result<()> {
    let target = resolved(conflicts)?;
    ensure!(target != resolved(master)?, "--conflicts を元データと同じにはできません");
    for dir in trees {
        ensure!(
            !target.starts_with(resolved(dir)?),
            "--conflicts を木の出力先の中にはできません"
        );
    }
    Ok(())
}

/// まだ無い dir を推測でたどると、その先の既存のリンクを見落とす。親 dir は既にあるものに限る。
fn resolved(path: &Path) -> Result<PathBuf> {
    if std::fs::symlink_metadata(path).is_ok() {
        return std::fs::canonicalize(path)
            .with_context(|| format!("{} を解決できません", path.display()));
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .with_context(|| format!("{} はファイル名ではありません", path.display()))?;
    Ok(std::fs::canonicalize(parent)
        .with_context(|| format!("{} がありません (先に作ってください)", parent.display()))?
        .join(name))
}

pub(super) fn parse_node_moves(text: &str, tree: usize) -> Result<BTreeMap<String, Node>> {
    let mut lines = text.lines();
    ensure!(
        lines.next() == Some("key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank"),
        "{NODE_MOVES} の見出しが違います"
    );
    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
    for line in lines {
        let cells: Vec<_> = line.split('\t').collect();
        ensure!(cells.len() == 9, "{NODE_MOVES} の列の数が違います: {line}");
        let node = nodes.entry(cells[0].to_owned()).or_insert_with(|| Node {
            ply: cells[1].to_owned(),
            remaining: 0,
            nodes: 0,
            moves: Vec::new(),
            tree,
        });
        node.remaining = cells[3].parse()?;
        node.nodes = cells[4].parse()?;
        node.moves.push(NodeMove {
            mv: cells[5].to_owned(),
            value: cells[6].parse()?,
        });
    }
    Ok(nodes)
}

/// 同じ局面は、その局面から先に伸ばせた段数 (remaining) の大きい木を使う。同じなら先に渡した木。
/// 木どうしで 1 位が違った局面の数を返す。
pub(super) fn merge(into: &mut BTreeMap<String, Node>, from: BTreeMap<String, Node>) -> usize {
    let mut disagreements = 0;
    for (key, node) in from {
        match into.get(&key) {
            Some(old) => {
                if old.moves.first().map(|m| &m.mv) != node.moves.first().map(|m| &m.mv) {
                    disagreements += 1;
                }
                if node.remaining > old.remaining {
                    into.insert(key, node);
                }
            }
            None => {
                into.insert(key, node);
            }
        }
    }
    disagreements
}

pub(super) fn parse_master(text: &str) -> Result<Vec<Row>> {
    let mut lines = text.lines();
    ensure!(lines.next() == Some(COLUMNS.join("\t").as_str()), "元データの見出しが違います");
    lines
        .enumerate()
        .map(|(i, line)| {
            let cells: Vec<String> = line.split('\t').map(str::to_owned).collect();
            ensure!(cells.len() == COLUMNS.len(), "元データの {} 行目の列の数が違います", i + 2);
            ensure!(
                ROLES.contains(&cells[index("role")].as_str()),
                "元データの {} 行目の role が不正です",
                i + 2
            );
            Ok(Row(cells))
        })
        .collect()
}

pub(super) fn format_master(rows: &[Row]) -> String {
    let mut text = COLUMNS.join("\t");
    text.push('\n');
    for row in rows {
        text.push_str(&row.0.join("\t"));
        text.push('\n');
    }
    text
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Conflict {
    pub key: String,
    pub ply: String,
    pub main: String,
    pub source: String,
    pub tier: String,
    pub main_value: Option<i32>,
    pub best: String,
    pub value: i32,
    pub tree: usize,
}
impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let side = self.key.split(' ').nth(1).unwrap_or("-");
        let main_value = self.main_value.map_or_else(|| "-".into(), |v| v.to_string());
        write!(
            f,
            "{}\t{}\t{side}\t{}\t{}\t{}\t{main_value}\t{}\t{}",
            self.key, self.ply, self.main, self.source, self.tier, self.best, self.value
        )
    }
}

/// 元データの行に木の結果を当てる。評価値は手番の側から。
pub(super) fn register(
    mut rows: Vec<Row>,
    nodes: &BTreeMap<String, Node>,
    options: &Options,
) -> Result<(Vec<Row>, Vec<Conflict>, Summary)> {
    let mut summary = Summary::default();
    let mut conflicts = Vec::new();
    let mut by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        by_key.entry(row.get("key").to_owned()).or_default().push(i);
    }
    let suffix = if options.note.is_empty() {
        String::new()
    } else {
        format!("。{}", options.note)
    };
    for (key, node) in nodes {
        let side = key.split(' ').nth(1).context("局面の手番がありません")?.to_owned();
        let existing = by_key.get(key).cloned().unwrap_or_default();
        let find =
            |rows: &[Row], mv: &str| existing.iter().copied().find(|&i| rows[i].get("move") == mv);
        let playable: Vec<&NodeMove> = node
            .moves
            .iter()
            .filter(|m| {
                let record = find(&rows, &m.mv).is_some_and(|i| rows[i].get("role") == "record");
                if record {
                    summary.record_skipped += 1;
                }
                !record
            })
            .collect();
        let Some(best) = playable.first().copied() else {
            continue;
        };
        let alts: Vec<&NodeMove> = playable[1..]
            .iter()
            .copied()
            .filter(|m| best.value - m.value < options.alt_gap)
            .collect();
        let nodes_text = format_nodes(node.nodes);
        let new_row = |mv: &NodeMove, role: &str, why: String| {
            Row(vec![
                key.clone(),
                node.ply.clone(),
                mv.mv.clone(),
                role.into(),
                "read".into(),
                mv.value.to_string(),
                nodes_text.clone(),
                String::new(),
                "wide".into(),
                options.since.into(),
                format!("{why}{suffix}"),
            ])
        };
        let first_new = rows.len();
        let main = existing.iter().copied().find(|&i| rows[i].get("role") == "main");
        let curated = existing.iter().any(|&i| rows[i].get("tier") == "curated");
        match main {
            None => {
                if let Some(i) = find(&rows, &best.mv) {
                    bail!("{key}: main の無い局面に {} の行があります", rows[i].get("move"));
                }
                rows.push(new_row(best, "main", "木の伝播後の 1 位".into()));
                *summary.added_main.entry(side.clone()).or_default() += 1;
            }
            Some(m) if rows[m].get("move") == best.mv => {}
            Some(m)
                if READ_SOURCES.contains(&rows[m].get("source"))
                    && rows[m].get("tier") == "wide"
                    && !curated =>
            {
                rows[m].set("role", "alt".into());
                let note = append(
                    rows[m].get("note"),
                    format!("{}: 木の伝播後の 1 位でないので main から alt", options.since),
                );
                rows[m].set("note", note);
                match find(&rows, &best.mv) {
                    Some(i) => {
                        rows[i].set("role", "main".into());
                        let note = append(
                            rows[i].get("note"),
                            format!("{}: 木の伝播後の 1 位なので alt から main", options.since),
                        );
                        rows[i].set("note", note);
                    }
                    None => rows.push(new_row(best, "main", "木の伝播後の 1 位".into())),
                }
                summary.replaced_main += 1;
            }
            Some(m) => {
                let main_move = rows[m].get("move");
                conflicts.push(Conflict {
                    key: key.clone(),
                    ply: node.ply.clone(),
                    main: main_move.into(),
                    source: rows[m].get("source").into(),
                    tier: rows[m].get("tier").into(),
                    main_value: node.moves.iter().find(|x| x.mv == main_move).map(|x| x.value),
                    best: best.mv.clone(),
                    value: best.value,
                    tree: node.tree,
                });
                summary.conflicts += 1;
                if find(&rows, &best.mv).is_none() {
                    rows.push(new_row(best, "alt", "木の伝播後の 1 位 (main と食い違い)".into()));
                    *summary.added_alt.entry(side.clone()).or_default() += 1;
                }
            }
        }
        let present: BTreeSet<String> =
            rows[first_new..].iter().map(|r| r.get("move").to_owned()).collect();
        for alt in alts {
            if find(&rows, &alt.mv).is_none() && !present.contains(&alt.mv) {
                rows.push(new_row(
                    alt,
                    "alt",
                    format!("木の伝播後で 1 位との差 {}", best.value - alt.value),
                ));
                *summary.added_alt.entry(side.clone()).or_default() += 1;
            }
        }
        for &i in &existing {
            let row = &rows[i];
            if row.get("role") == "record" || !READ_SOURCES.contains(&row.get("source")) {
                continue;
            }
            if let Some(m) = node.moves.iter().find(|m| m.mv == row.get("move")) {
                let value = m.value.to_string();
                if row.get("evals") != value || row.get("nodes") != nodes_text {
                    rows[i].set("evals", value);
                    rows[i].set("nodes", nodes_text.clone());
                    summary.evals_updated += 1;
                }
            }
        }
    }
    rows.sort_by(|a, b| {
        let role = |r: &Row| ROLES.iter().position(|x| *x == r.get("role"));
        (a.get("key"), role(a), a.get("move")).cmp(&(b.get("key"), role(b), b.get("move")))
    });
    validate(&rows)?;
    Ok((rows, conflicts, summary))
}

fn append(note: &str, text: String) -> String {
    if note.is_empty() {
        text
    } else {
        format!("{note}。{text}")
    }
}

/// 局面ごとに main がちょうど 1 つ (丁寧な定跡の行だけでも 1 つ) で、同じ手の行が重ならないこと。
fn validate(rows: &[Row]) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut groups: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        ensure!(
            seen.insert((row.get("key"), row.get("move"))),
            "同じ手の行が重なっています: {} {}",
            row.get("key"),
            row.get("move")
        );
        groups.entry(row.get("key")).or_default().push(row);
    }
    for (key, entries) in groups {
        let curated: Vec<_> =
            entries.iter().copied().filter(|r| r.get("tier") == "curated").collect();
        for subset in [entries, curated] {
            if subset.iter().any(|r| r.get("role") != "record") {
                ensure!(
                    subset.iter().filter(|r| r.get("role") == "main").count() == 1,
                    "{key}: main がちょうど 1 つではありません"
                );
            }
        }
    }
    Ok(())
}

/// 読みのノード数を元データの書き方 (1e9・2.3e8) にする。
pub(super) fn format_nodes(nodes: u64) -> String {
    if nodes == 0 {
        return "0".into();
    }
    let text = format!("{:.2e}", nodes as f64);
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
    format!("{mantissa}e{exponent}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: &str = "pos1 b -";
    const W: &str = "pos2 w -";

    fn row(key: &str, mv: &str, role: &str, source: &str, tier: &str, evals: &str) -> String {
        format!("{key}\t10\t{mv}\t{role}\t{source}\t{evals}\t7e9\t\t{tier}\told\tn")
    }
    fn master(rows: &[String]) -> Vec<Row> {
        let mut text = COLUMNS.join("\t");
        for r in rows {
            text.push('\n');
            text.push_str(r);
        }
        parse_master(&text).unwrap()
    }
    fn node(moves: &[(&str, i32)], remaining: usize, tree: usize) -> Node {
        Node {
            ply: "12".into(),
            remaining,
            nodes: 1_000_000_000,
            moves: moves
                .iter()
                .map(|(m, v)| NodeMove {
                    mv: (*m).into(),
                    value: *v,
                })
                .collect(),
            tree,
        }
    }
    fn options() -> Options<'static> {
        Options {
            since: "tag",
            note: "plan",
            alt_gap: 40,
        }
    }
    fn find<'a>(rows: &'a [Row], key: &str, mv: &str) -> Option<&'a Row> {
        rows.iter().find(|r| r.get("key") == key && r.get("move") == mv)
    }

    #[test]
    fn new_positions_of_both_sides_get_main_and_close_alts() {
        let nodes = BTreeMap::from([
            (B.into(), node(&[("a", 50), ("b", 11), ("c", 10)], 2, 0)),
            (W.into(), node(&[("x", -80), ("y", -100)], 0, 0)),
        ]);
        let (rows, conflicts, summary) = register(master(&[]), &nodes, &options()).unwrap();
        assert!(conflicts.is_empty());
        let a = find(&rows, B, "a").unwrap();
        assert_eq!(
            (a.get("role"), a.get("source"), a.get("evals"), a.get("nodes")),
            ("main", "read", "50", "1e9")
        );
        assert_eq!((a.get("tier"), a.get("since"), a.get("ply")), ("wide", "tag", "12"));
        assert!(a.get("note").ends_with("。plan"));
        assert_eq!(find(&rows, B, "b").unwrap().get("role"), "alt");
        assert!(find(&rows, B, "c").is_none());
        assert_eq!(find(&rows, W, "x").unwrap().get("role"), "main");
        assert_eq!(find(&rows, W, "y").unwrap().get("role"), "alt");
        assert_eq!(summary.added_main, BTreeMap::from([("b".into(), 1), ("w".into(), 1)]));
        assert_eq!(summary.added_alt, BTreeMap::from([("b".into(), 1), ("w".into(), 1)]));
    }

    #[test]
    fn read_main_is_replaced_but_owner_and_curated_mains_become_conflicts() {
        let k3 = "pos3 b -";
        let rows = master(&[
            row(B, "old", "main", "deep", "wide", "1,2"),
            row(B, "new", "alt", "read", "wide", "5"),
            row(W, "pinned", "main", "team", "wide", "9"),
            row(k3, "careful", "main", "read", "curated", "3"),
        ]);
        let nodes = BTreeMap::from([
            (B.into(), node(&[("new", 30), ("old", -20)], 1, 0)),
            (W.into(), node(&[("tree", 10), ("pinned", 0)], 1, 1)),
            (k3.into(), node(&[("better", 60), ("careful", 0)], 1, 0)),
        ]);
        let (rows, conflicts, summary) = register(rows, &nodes, &options()).unwrap();
        assert_eq!(find(&rows, B, "new").unwrap().get("role"), "main");
        let old = find(&rows, B, "old").unwrap();
        assert_eq!((old.get("role"), old.get("evals"), old.get("since")), ("alt", "-20", "old"));
        let pinned = find(&rows, W, "pinned").unwrap();
        assert_eq!((pinned.get("role"), pinned.get("evals")), ("main", "9"));
        assert_eq!(find(&rows, W, "tree").unwrap().get("role"), "alt");
        assert_eq!(find(&rows, k3, "careful").unwrap().get("role"), "main");
        assert_eq!(find(&rows, k3, "better").unwrap().get("role"), "alt");
        assert_eq!(summary.replaced_main, 1);
        assert_eq!(conflicts.len(), 2);
        assert_eq!(
            (conflicts[0].main.as_str(), conflicts[0].main_value, conflicts[0].best.as_str()),
            ("pinned", Some(0), "tree")
        );
        assert_eq!(conflicts[0].tree, 1);
        assert_eq!(conflicts[1].main, "careful");
    }

    #[test]
    fn curated_record_rows_also_keep_the_main() {
        let rows = master(&[
            row(B, "old", "main", "read", "wide", "1"),
            row(B, "never", "record", "read", "curated", "-30000"),
        ]);
        let nodes = BTreeMap::from([(B.into(), node(&[("new", 30), ("old", -20)], 1, 0))]);
        let (rows, conflicts, summary) = register(rows, &nodes, &options()).unwrap();
        assert_eq!(find(&rows, B, "old").unwrap().get("role"), "main");
        assert_eq!(find(&rows, B, "new").unwrap().get("role"), "alt");
        assert_eq!((conflicts.len(), summary.replaced_main), (1, 0));
    }

    #[test]
    fn conflicts_path_is_compared_by_its_real_location() {
        let dir = tempfile::tempdir().unwrap();
        let master = dir.path().join("moves.tsv");
        std::fs::write(&master, "x").unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        let trees = [tree.clone()];
        let mut bad = vec![
            dir.path().join("tree/../moves.tsv"),
            tree.join("conflicts.tsv"),
            dir.path().join("tree/./x.tsv"),
        ];
        #[cfg(unix)]
        {
            let link = dir.path().join("link.tsv");
            std::os::unix::fs::symlink(&master, &link).unwrap();
            bad.push(link);
        }
        for bad in bad {
            assert!(check_conflicts_path(&bad, &master, &trees).is_err(), "{}", bad.display());
        }
        check_conflicts_path(&dir.path().join("conflicts.tsv"), &master, &trees).unwrap();
        for bad in [
            "new/conflicts.tsv",
            "new/../reports/conflicts.tsv",
            "tree/new/conflicts.tsv",
            "new/../moves.tsv",
            "new/../tree/x.tsv",
        ] {
            assert!(check_conflicts_path(&dir.path().join(bad), &master, &trees).is_err(), "{bad}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&tree, dir.path().join("tree_link")).unwrap();
            for bad in [
                "new/../tree_link/node_moves.tsv",
                "tree_link/node_moves.tsv",
            ] {
                assert!(
                    check_conflicts_path(&dir.path().join(bad), &master, &trees).is_err(),
                    "{bad}"
                );
            }
        }
        assert_eq!(std::fs::read_to_string(&master).unwrap(), "x");
    }

    #[test]
    fn run_checks_trees_and_writes_the_master_only_with_apply() {
        let dir = tempfile::tempdir().unwrap();
        let master_path = dir.path().join("moves.tsv");
        let original = format_master(&master(&[row(B, "pinned", "main", "pin", "wide", "")]));
        std::fs::write(&master_path, &original).unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(tree.join("STATUS"), "DONE\n").unwrap();
        std::fs::write(
            tree.join(NODE_MOVES),
            format!("key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n{B}\t3\t0\t2\t1000000000\tbest\t40\t1\t1\n{W}\t4\t1\t1\t1000000000\tw1\t-5\t1\t1\n"),
        )
        .unwrap();
        let args = |trees: Vec<PathBuf>, apply| RegisterArgs {
            master: master_path.clone(),
            since: "tag".into(),
            note: String::new(),
            alt_gap: 40,
            conflicts: dir.path().join("conflicts.tsv"),
            apply,
            trees,
        };
        run(args(vec![tree.clone()], false)).unwrap();
        assert_eq!(std::fs::read_to_string(&master_path).unwrap(), original);
        let conflicts = std::fs::read_to_string(dir.path().join("conflicts.tsv")).unwrap();
        assert_eq!(conflicts.lines().count(), 2);
        assert!(
            conflicts
                .lines()
                .nth(1)
                .unwrap()
                .starts_with(&format!("{B}\t3\tb\tpinned\tpin\twide\t-\tbest\t40\t"))
        );
        run(args(vec![tree.clone()], true)).unwrap();
        let rows = parse_master(&std::fs::read_to_string(&master_path).unwrap()).unwrap();
        assert_eq!(find(&rows, W, "w1").unwrap().get("role"), "main");
        assert_eq!(
            find(&rows, B, "best").unwrap().get("note"),
            "木の伝播後の 1 位 (main と食い違い)"
        );

        let missing = dir.path().join("typo");
        assert!(run(args(vec![missing.clone()], false)).is_err());
        assert!(!missing.exists());
        std::fs::write(tree.join("STATUS"), "NOT_READY\n").unwrap();
        assert!(run(args(vec![tree.clone()], false)).is_err());
        std::fs::write(tree.join("STATUS"), "DONE\n").unwrap();
        std::fs::remove_file(tree.join(NODE_MOVES)).unwrap();
        assert!(run(args(vec![tree], false)).is_err());
    }

    #[test]
    fn record_moves_are_never_added_and_the_next_move_becomes_main() {
        let rows = master(&[row(B, "bad", "record", "read", "wide", "-30000")]);
        let nodes = BTreeMap::from([(B.into(), node(&[("bad", 90), ("ok", 80)], 1, 0))]);
        let (rows, _, summary) = register(rows, &nodes, &options()).unwrap();
        assert_eq!(find(&rows, B, "bad").unwrap().get("role"), "record");
        assert_eq!(find(&rows, B, "bad").unwrap().get("evals"), "-30000");
        assert_eq!(find(&rows, B, "ok").unwrap().get("role"), "main");
        assert_eq!(summary.record_skipped, 1);
    }

    #[test]
    fn deeper_tree_wins_and_rows_are_sorted_like_the_python_tools() {
        let mut nodes = BTreeMap::from([(B.into(), node(&[("a", 10)], 1, 0))]);
        let deeper = BTreeMap::from([(B.into(), node(&[("b", 20), ("a", 0)], 3, 1))]);
        assert_eq!(merge(&mut nodes, deeper), 1);
        assert_eq!(nodes[B].moves[0].mv, "b");
        let shallower = BTreeMap::from([(B.into(), node(&[("c", 99)], 0, 2))]);
        merge(&mut nodes, shallower);
        assert_eq!(nodes[B].tree, 1);
        let (rows, _, _) = register(
            master(&[]),
            &nodes,
            &Options {
                alt_gap: 0,
                ..options()
            },
        )
        .unwrap();
        let order: Vec<_> =
            rows.iter().map(|r| (r.get("key"), r.get("role"), r.get("move"))).collect();
        assert_eq!(order, [(B, "main", "b")]);
        let text = format_master(&rows);
        assert_eq!(format_master(&parse_master(&text).unwrap()), text);
    }

    #[test]
    fn invalid_master_is_rejected() {
        let rows = master(&[row(B, "a", "alt", "read", "wide", "1")]);
        let nodes = BTreeMap::from([(B.into(), node(&[("a", 1)], 1, 0))]);
        assert!(register(rows, &nodes, &options()).is_err());
        assert_eq!(format_nodes(1_000_000_000), "1e9");
        assert_eq!(format_nodes(230_000_000), "2.3e8");
    }
}
