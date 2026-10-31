//! 木の出力 (node_moves.tsv) から定跡の元データの表に手を登録する。

use super::journal::LockedFile;
use super::master_table::{ROLES, Row, format_master, format_nodes, parse_master, validate};
use super::output::NODE_MOVES;
use crate::common::io::write_atomic;
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

use super::schema::Schema;

#[derive(Args, Debug)]
pub(super) struct RegisterArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// 指紋の無い木の読みを照合する台帳 (読み取り専用)。候補を全て繰り返し指定
    #[arg(long)]
    journal: Vec<PathBuf>,
    /// 足す行の since 列
    #[arg(long)]
    since: String,
    /// 足す行の note 列の末尾に付ける文
    #[arg(long, default_value = "")]
    note: String,
    /// main からこの値未満の差の手を alt にする
    #[arg(long, default_value_t = 40)]
    alt_gap: i32,
    /// main を残して木の 1 位と食い違った局面の一覧 (TSV)。まだ無いファイルに限る
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
    pub value_kind: String,
    pub cond: String,
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
    /// 足す行の source 列。
    pub source: &'a str,
    /// 足す行の lineage 列。
    pub lineage: &'a str,
    pub schema: &'a Schema,
}

pub(super) fn run(args: RegisterArgs) -> Result<()> {
    ensure!(!args.since.trim().is_empty(), "--since を指定してください");
    for text in [&args.since, &args.note] {
        ensure!(!text.contains(['\t', '\r', '\n']), "since・note にタブや改行は使えません");
    }
    ensure!(args.alt_gap >= 0, "--alt-gap は 0 以上にしてください");
    // 木を読む前に止める。書き出しの最後でも create_new で同じことを確かめる。
    ensure!(
        std::fs::symlink_metadata(&args.conflicts).is_err(),
        "{} は既にあります (--conflicts は新しいファイルにしてください)",
        args.conflicts.display()
    );
    let schema = Schema::load(args.schema.as_deref())?;
    let mut unknown = 0;
    let mut nodes = BTreeMap::new();
    let mut disagreements = 0;
    let mut fingerprints = Vec::new();
    let mut readings = Vec::new();
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
        let fingerprint = tree_fingerprint(dir)?;
        readings.push(if fingerprint.is_none() && !args.journal.is_empty() {
            tree_readings(dir)?
        } else {
            Vec::new()
        });
        fingerprints.push(fingerprint);
        disagreements += merge(&mut nodes, parse_node_moves(&text, tree)?);
    }
    let matched = super::journal::match_fingerprints(&args.journal, &readings)?;
    let mut conditions = Vec::new();
    for (fingerprint, matched) in fingerprints.into_iter().zip(matched) {
        let cond = fingerprint
            .or(matched)
            .map_or_else(|| Ok("unknown".into()), |fp| schema.condition(&fp))?;
        unknown += usize::from(cond == "unknown");
        conditions.push(cond);
    }
    for node in nodes.values_mut() {
        for mv in &mut node.moves {
            mv.cond = conditions[node.tree].clone();
        }
    }
    let text = std::fs::read_to_string(&args.master)
        .with_context(|| format!("{} を読めません", args.master.display()))?;
    let rows = parse_master(&text)?;
    for row in &rows {
        schema.validate_row(row)?;
    }
    let before = rows.len();
    let options = Options {
        since: &args.since,
        note: &args.note,
        alt_gap: args.alt_gap,
        source: "read",
        lineage: "",
        schema: &schema,
    };
    let (rows, conflicts, summary) = register(rows, &nodes, &options)?;
    let mut table = String::from(
        "key\tply\tside\tmain\tmain_source\tmain_tier\tmain_tree_value\ttree_best\ttree_value\ttree\n",
    );
    for c in &conflicts {
        writeln!(table, "{c}\t{}", args.trees[c.tree].display())?;
    }
    write_new(&args.conflicts, &table)?;
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
    println!("cond=unknown の木: {unknown}");
    if args.apply {
        write_atomic(&args.master, &format_master(&rows))?;
        println!("書き換えました: 行 {before} -> {}", rows.len());
    } else {
        println!("確認だけ (--apply なし): 行 {before} -> {}", rows.len());
    }
    Ok(())
}

/// 食い違いの一覧は確認だけの実行でも書くので、まだ無いファイルにだけ書く。既にあるファイル (元データ・
/// 木の出力) は、リンク・`..`・大小文字を区別しない名前などどの書き方で指しても上書きしない。
fn write_new(path: &Path, text: &str) -> Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| {
            format!("{} に書けません (既にあるファイルには書きません)", path.display())
        })?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(())
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
        ensure!(matches!(cells[7], "0" | "1"), "{NODE_MOVES}: expanded は 0 / 1 にしてください");
        node.moves.push(NodeMove {
            cond: "unknown".into(),
            mv: cells[5].to_owned(),
            value: cells[6].parse()?,
            value_kind: if cells[7] == "1" && node.remaining >= 1 {
                format!("tree:{}", node.remaining)
            } else {
                "single".into()
            },
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
            Row::from_fields(&[
                ("key", key.clone()),
                ("ply", node.ply.clone()),
                ("move", mv.mv.clone()),
                ("role", role.into()),
                ("source", options.source.into()),
                ("evals", mv.value.to_string()),
                ("nodes", nodes_text.clone()),
                ("cond", mv.cond.clone()),
                ("value_kind", mv.value_kind.clone()),
                ("lineage", options.lineage.into()),
                ("tier", "wide".into()),
                ("since", options.since.into()),
                ("note", format!("{why}{suffix}")),
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
                if options.schema.replaceable(rows[m].get("source"))
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
            if row.get("role") == "record" || !options.schema.replaceable(row.get("source")) {
                continue;
            }
            if let Some(m) = node.moves.iter().find(|m| m.mv == row.get("move")) {
                let value = m.value.to_string();
                // `+5` と `5` のように表記だけが違う値で、分かっている条件を失わない。
                let same_value = row.get("evals").parse::<i64>().ok() == Some(i64::from(m.value))
                    && row.get("nodes") == nodes_text;
                let keep_known = |old: &str, new: &str| {
                    if same_value && new == "unknown" && !old.is_empty() {
                        old.to_owned()
                    } else {
                        new.to_owned()
                    }
                };
                let cond = keep_known(row.get("cond"), &m.cond);
                let kind = keep_known(row.get("value_kind"), &m.value_kind);
                if row.get("evals") != value
                    || row.get("nodes") != nodes_text
                    || row.get("cond") != cond
                    || row.get("value_kind") != kind
                {
                    rows[i].set("evals", value);
                    rows[i].set("nodes", nodes_text.clone());
                    rows[i].set("cond", cond);
                    rows[i].set("value_kind", kind);
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
    for row in &rows {
        options.schema.validate_row(row)?;
    }
    Ok((rows, conflicts, summary))
}

fn tree_fingerprint(dir: &Path) -> Result<Option<String>> {
    let path = dir.join("registration_status.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let status: serde_json::Value = serde_json::from_slice(&bytes)?;
            status
                .get("fingerprint")
                .map(|fp| {
                    fp.as_str().context("fingerprint は文字列にしてください").map(str::to_owned)
                })
                .transpose()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn tree_readings(dir: &Path) -> Result<Vec<super::journal::Reading>> {
    use std::io::BufRead;
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name
            .strip_prefix('d')
            .and_then(|s| s.strip_suffix(".jsonl"))
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    let mut readings = Vec::new();
    for path in paths {
        for line in std::io::BufReader::new(std::fs::File::open(path)?).lines() {
            readings.push(serde_json::from_str(&line?)?);
        }
    }
    Ok(readings)
}

#[cfg(test)]
fn tree_condition(dir: &Path, journal: Option<&Path>, schema: &Schema) -> Result<String> {
    let fingerprint = match tree_fingerprint(dir)? {
        Some(fp) => Some(fp),
        None => match journal {
            Some(path) => {
                super::journal::match_fingerprints(&[path.to_owned()], &[tree_readings(dir)?])?
                    .remove(0)
            }
            None => None,
        },
    };
    fingerprint.map_or_else(|| Ok("unknown".into()), |fp| schema.condition(&fp))
}

fn append(note: &str, text: String) -> String {
    if note.is_empty() {
        text
    } else {
        format!("{note}。{text}")
    }
}

#[cfg(test)]
mod tests {
    use super::super::master_table::COLUMNS;
    use super::*;

    const B: &str = "pos1 b -";
    const W: &str = "pos2 w -";

    fn row(key: &str, mv: &str, role: &str, source: &str, tier: &str, evals: &str) -> String {
        crate::book_tree::master_table::test_line(&[
            ("key", key.into()),
            ("ply", "10".into()),
            ("move", mv.into()),
            ("role", role.into()),
            ("source", source.into()),
            ("evals", evals.into()),
            ("nodes", "7e9".into()),
            ("tier", tier.into()),
            ("since", "old".into()),
            ("note", "n".into()),
        ])
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
                    cond: "unknown".into(),
                    mv: (*m).into(),
                    value: *v,
                    value_kind: if remaining >= 1 {
                        format!("tree:{remaining}")
                    } else {
                        "single".into()
                    },
                })
                .collect(),
            tree,
        }
    }
    fn options() -> Options<'static> {
        static SCHEMA: std::sync::LazyLock<Schema> = std::sync::LazyLock::new(Schema::default);
        Options {
            schema: &SCHEMA,
            since: "tag",
            note: "plan",
            alt_gap: 40,
            source: "read",
            lineage: "",
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
    fn conflicts_are_written_only_to_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let master = dir.path().join("moves.tsv");
        std::fs::write(&master, "x").unwrap();
        assert!(write_new(&master, "y").is_err());
        #[cfg(unix)]
        {
            let link = dir.path().join("dangling.tsv");
            std::os::unix::fs::symlink(dir.path().join("none"), &link).unwrap();
            assert!(write_new(&link, "y").is_err());
            assert!(!dir.path().join("none").exists());
        }
        assert_eq!(std::fs::read_to_string(&master).unwrap(), "x");
        write_new(&dir.path().join("conflicts.tsv"), "z").unwrap();
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
            schema: None,
            journal: Vec::new(),
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
        assert!(run(args(vec![tree.clone()], true)).is_err());
        assert_eq!(std::fs::read_to_string(&master_path).unwrap(), original);
        std::fs::remove_file(dir.path().join("conflicts.tsv")).unwrap();
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
    fn deeper_tree_wins_and_rows_are_sorted_by_key_role_and_move() {
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
    #[test]
    fn expanded_and_metadata_changes_are_registered_without_value_changes() {
        let table = format!(
            "key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n{B}\t1\t0\t2\t100\ta\t10\t1\t1\n{B}\t1\t0\t2\t100\tb\t9\t0\t2\n{W}\t2\t2\t0\t100\tx\t8\t1\t1\n"
        );
        let mut nodes = parse_node_moves(&table, 0).unwrap();
        assert_eq!(nodes[B].moves[0].value_kind, "tree:2");
        assert_eq!(nodes[B].moves[1].value_kind, "single");
        assert_eq!(nodes[W].moves[0].value_kind, "single");
        assert!(parse_node_moves(&table.replace("\t1\t1\n", "\t2\t1\n"), 0).is_err());
        let (rows, _, _) = register(vec![], &nodes, &options()).unwrap();
        assert!(rows.iter().all(|r| r.get("lineage").is_empty()));
        for node in nodes.values_mut() {
            for mv in &mut node.moves {
                mv.cond = "abcd1234".into();
            }
        }
        let (rows, _, summary) = register(rows, &nodes, &options()).unwrap();
        assert_eq!(summary.evals_updated, 3);
        nodes.get_mut(B).unwrap().moves[0].value_kind = "tree:3".into();
        let (rows, _, summary) = register(rows, &nodes, &options()).unwrap();
        assert_eq!(summary.evals_updated, 1);
        assert_eq!(find(&rows, B, "a").unwrap().get("value_kind"), "tree:3");
        let (_, _, summary) = register(rows, &nodes, &options()).unwrap();
        assert_eq!(summary.evals_updated, 0);
    }

    #[test]
    fn unchanged_values_preserve_known_metadata_but_changed_values_replace_it() {
        let mut nodes = BTreeMap::from([(B.into(), node(&[("a", 10)], 2, 0))]);
        nodes.get_mut(B).unwrap().moves[0].cond = "known".into();
        let (original, _, _) = register(vec![], &nodes, &options()).unwrap();
        for (cond, kind, expected_cond, expected_kind, updates) in [
            ("unknown", "tree:2", "known", "tree:2", 0),
            ("known", "unknown", "known", "tree:2", 0),
            ("unknown", "unknown", "known", "tree:2", 0),
            ("other", "unknown", "other", "tree:2", 1),
            ("unknown", "single", "known", "single", 1),
        ] {
            let mv = &mut nodes.get_mut(B).unwrap().moves[0];
            mv.cond = cond.into();
            mv.value_kind = kind.into();
            let (rows, _, summary) = register(original.clone(), &nodes, &options()).unwrap();
            assert_eq!(rows[0].get("cond"), expected_cond);
            assert_eq!(rows[0].get("value_kind"), expected_kind);
            assert_eq!(summary.evals_updated, updates);
        }
        for change_nodes in [false, true] {
            let n = nodes.get_mut(B).unwrap();
            n.moves[0].cond = "unknown".into();
            n.moves[0].value_kind = "unknown".into();
            n.moves[0].value = if change_nodes { 10 } else { 11 };
            n.nodes = if change_nodes {
                2_000_000_000
            } else {
                1_000_000_000
            };
            let (rows, _, summary) = register(original.clone(), &nodes, &options()).unwrap();
            assert_eq!(rows[0].get("cond"), "unknown");
            assert_eq!(rows[0].get("value_kind"), "unknown");
            assert_eq!(summary.evals_updated, 1);
        }
        let n = nodes.get_mut(B).unwrap();
        n.moves[0].value = 10;
        n.nodes = 1_000_000_000;
        let mut plus = original.clone();
        plus[0].set("evals", "+10".into());
        let (rows, _, _) = register(plus, &nodes, &options()).unwrap();
        assert_eq!(rows[0].get("evals"), "10");
        assert_eq!(rows[0].get("cond"), "known");
        assert_eq!(rows[0].get("value_kind"), "tree:2");
    }

    #[test]
    fn tree_status_resolves_names_and_requires_registered_fingerprints() {
        let dir = tempfile::tempdir().unwrap();
        let schema = Schema::default();
        assert_eq!(tree_condition(dir.path(), None, &schema).unwrap(), "unknown");
        let fp = "a".repeat(64);
        std::fs::write(
            dir.path().join("registration_status.json"),
            serde_json::json!({"ready":true,"unresolved":{},"fingerprint":fp}).to_string(),
        )
        .unwrap();
        assert_eq!(tree_condition(dir.path(), None, &schema).unwrap(), "aaaaaaaa");
        crate::book_tree::schema::test_schema(dir.path());
        std::fs::write(
            dir.path().join("sources.tsv"),
            "source\tdescription\treplaceable\nread\t読み\t1\n",
        )
        .unwrap();
        let schema = Schema::load(Some(dir.path())).unwrap();
        assert!(
            tree_condition(dir.path(), None, &schema)
                .unwrap_err()
                .to_string()
                .contains("追加")
        );
        let path = dir.path().join("conditions.tsv");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(&format!(
            "{}\n",
            ["named".into(), fp]
                .into_iter()
                .chain(std::iter::repeat_n(String::new(), 11))
                .collect::<Vec<_>>()
                .join("\t")
        ));
        std::fs::write(path, text).unwrap();
        let schema = Schema::load(Some(dir.path())).unwrap();
        assert_eq!(tree_condition(dir.path(), None, &schema).unwrap(), "named");
        std::fs::write(dir.path().join("registration_status.json"), r#"{"fingerprint":"bad"}"#)
            .unwrap();
        assert!(tree_condition(dir.path(), None, &schema).is_err());
    }

    #[test]
    fn schema_replaceable_controls_main_and_values() {
        let dir = tempfile::tempdir().unwrap();
        crate::book_tree::schema::test_schema(dir.path());
        std::fs::write(
            dir.path().join("sources.tsv"),
            "source\tdescription\treplaceable\ncustom\t読み\t1\nread\t保護\t0\n",
        )
        .unwrap();
        let schema = Schema::load(Some(dir.path())).unwrap();
        let options = Options {
            schema: &schema,
            ..options()
        };
        for (source, tier, replaced, updated) in [
            ("custom", "wide", 1, 1),
            ("custom", "curated", 0, 1),
            ("read", "wide", 0, 0),
        ] {
            let rows = master(&[row(B, "a", "main", source, tier, "10")]);
            let nodes = BTreeMap::from([(B.into(), node(&[("b", 30), ("a", 20)], 2, 0))]);
            let (_, _, summary) = register(rows, &nodes, &options).unwrap();
            assert_eq!(summary.replaced_main, replaced);
            assert_eq!(summary.evals_updated, updated);
        }
    }
    #[test]
    fn legacy_tree_journal_matching_is_read_only_and_conservative() {
        use crate::book_tree::journal::{Journal, Pv, Reading, Settings};
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        std::fs::write(&engine, "test engine").unwrap();
        let mut settings = Settings::new(&crate::book_tree::ReadingArgs {
            engine_options: vec![format!("EvalFile={}", engine.display())],
            engine,
            nodes: 100,
            multipv: 1,
            hash: 16,
        })
        .unwrap();
        let mut reading = Reading {
            tag: "test".into(),
            ply: 1,
            key: "4k4/9/9/9/9/9/9/9/4K4 b -".into(),
            nodes_limit: 100,
            nodes: Some(100),
            sec: 0.1,
            threads: 1,
            hash: 16,
            draw: BTreeMap::new(),
            multipv: vec![Pv {
                mv: "5i4i".into(),
                cp: 10,
                depth: 1,
            }],
            bestmove: "5i4i".into(),
        };
        let journal = dir.path().join("journal.jsonl");
        Journal::open(&journal, &settings.fingerprint)
            .unwrap()
            .append(reading.clone(), &settings, "test".into())
            .unwrap();
        let first = serde_json::to_string(&reading).unwrap();
        std::fs::write(dir.path().join("d0.jsonl"), format!("{first}\n")).unwrap();
        let schema = Schema::default();
        assert_eq!(
            tree_condition(dir.path(), Some(&journal), &schema).unwrap(),
            settings.fingerprint[..8]
        );
        settings.engine_sha256 = "another".into();
        settings.fingerprint = settings.computed_fingerprint().unwrap();
        reading.multipv[0].cp = 20;
        Journal::open(&journal, &settings.fingerprint)
            .unwrap()
            .append(reading.clone(), &settings, "test".into())
            .unwrap();
        std::fs::write(
            dir.path().join("d1.jsonl"),
            format!("{}\n", serde_json::to_string(&reading).unwrap()),
        )
        .unwrap();
        let before = std::fs::read(&journal).unwrap();
        assert_eq!(tree_condition(dir.path(), Some(&journal), &schema).unwrap(), "unknown");
        reading.multipv[0].cp = 21;
        std::fs::write(
            dir.path().join("d1.jsonl"),
            format!("{}\n", serde_json::to_string(&reading).unwrap()),
        )
        .unwrap();
        assert_eq!(tree_condition(dir.path(), Some(&journal), &schema).unwrap(), "unknown");
        assert_eq!(std::fs::read(&journal).unwrap(), before);
    }
}
