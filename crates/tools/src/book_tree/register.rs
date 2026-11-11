//! 木の出力 (node_moves.tsv) から定跡の元データの表に手を登録する。

mod keep;

use super::audit::Fill;
use super::journal::LockedFile;
use super::master_table::{ROLES, Row, format_master, format_nodes, parse_master, validate};
use super::model::child;
use super::output::NODE_MOVES;
use crate::book_backprop::DrawValues;
use crate::book_db::{position_from_sfen, strip_ply};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::audit::{reject_same_file, resolve, resolve_output};
use super::schema::Schema;
use super::switch::{self, SingleQueueArgs, SwitchRule};
pub(super) use keep::Kept;

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
    /// curated の行がある局面でも、main の source が置き換えてよいものなら main を木の 1 位に替える
    #[arg(long)]
    replace_curated: bool,
    /// 取り込まない局面の SFEN キーを 1 行に 1 つ書いたファイル
    #[arg(long, value_name = "FILE")]
    skip_keys: Option<PathBuf>,
    /// main を残して木の 1 位と食い違った局面の一覧 (TSV)。まだ無いファイルに限る
    #[arg(long)]
    conflicts: PathBuf,
    /// 元データを書き換える。省くと集計と食い違いの一覧だけを出す
    #[arg(long)]
    apply: bool,
    /// main を木の 1 位に替えるかの決め方。line は --keep-line・--keep-min-depth で元の main を残し、propagated は
    /// 登録後の元データを master audit と同じ伝播で解き、両方の値が手の先の局面から来て木の 1 位が上回る局面だけを
    /// 替え、片方の値が 1 回の読みなら替えない (--single-enqueue-out でその手を伸ばす木を出す)
    #[arg(long, value_enum, default_value_t = SwitchRule::Line)]
    switch_rule: SwitchRule,
    #[command(flatten)]
    single: SingleQueueArgs,
    /// 登録後の元データを master audit と同じ伝播で解き、元の main が木の 1 位をこの値以上上回る局面は
    /// main を替えずに食い違いの一覧に出す。0 で比べない (既定 20、--switch-rule line だけ)
    #[arg(long)]
    keep_line: Option<i32>,
    /// 元の main の値がこの数以上先の局面の読みから来たときだけ main を残す (0 なら元の main の 1 回の読みの値でも残す)
    /// (既定 1、--switch-rule line だけ)
    #[arg(long)]
    keep_min_depth: Option<usize>,
    /// 比べる伝播で master audit --bridge-replies と同じく相手の手番の局面を補う
    #[arg(long)]
    bridge_replies: bool,
    /// 比べる伝播で先手を自分として解くときの千日手の値 (master audit と揃える)
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    draw_value_black: i32,
    /// 比べる伝播で後手を自分として解くときの千日手の値 (master audit と揃える)
    #[arg(long, default_value_t = 70, allow_hyphen_values = true)]
    draw_value_white: i32,
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
    /// `replaced_main` のうち、curated の main を替えた局面。
    pub replaced_curated: usize,
    pub evals_updated: usize,
    pub record_skipped: usize,
    pub bench_revived: usize,
    pub bench_protected: usize,
    pub bench_curated_without_main: usize,
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
    /// curated の局面でも main を木の 1 位に替える。
    pub replace_curated: bool,
    pub revive_bench: bool,
    /// 替えずに main を残す局面。
    pub kept: Option<&'a BTreeMap<String, Kept>>,
}

pub(super) fn run(args: RegisterArgs) -> Result<()> {
    ensure!(!args.since.trim().is_empty(), "--since を指定してください");
    for text in [&args.since, &args.note] {
        ensure!(!text.contains(['\t', '\r', '\n']), "since・note にタブや改行は使えません");
    }
    ensure!(args.alt_gap >= 0, "--alt-gap は 0 以上にしてください");
    let propagated = args.switch_rule == SwitchRule::Propagated;
    ensure!(
        !propagated || (args.keep_line.is_none() && args.keep_min_depth.is_none()),
        "--keep-line・--keep-min-depth は --switch-rule line と一緒に使います"
    );
    ensure!(
        propagated || args.single.single_enqueue_out.is_none(),
        "--single-enqueue-out は --switch-rule propagated と一緒に使います"
    );
    let keep_line = args.keep_line.unwrap_or(20);
    ensure!(keep_line >= 0, "--keep-line は 0 以上にしてください");
    if let Some(out) = &args.single.single_enqueue_out {
        args.single.check()?;
        let mut inputs = vec![
            ("--master", args.master.as_path()),
            ("--conflicts", args.conflicts.as_path()),
        ];
        inputs.extend(args.single.exclude_queue.iter().map(|p| ("--exclude-queue", p.as_path())));
        inputs.extend(args.journal.iter().map(|p| ("--journal", p.as_path())));
        inputs.extend(args.skip_keys.iter().map(|p| ("--skip-keys", p.as_path())));
        let schema_paths = args.schema.as_deref().map(super::schema::paths);
        inputs.extend(schema_paths.iter().flatten().map(|p| ("--schema", p.as_path())));
        reject_same_file(out, &inputs).context("--single-enqueue-out の出力先")?;
        // 木の出力先の中のファイル (node_moves.tsv など) を書き換えない。
        let resolved = resolve_output(out)?;
        for tree in &args.trees {
            ensure!(
                !resolved.starts_with(resolve(tree)),
                "--single-enqueue-out の出力先 {} が木の出力先 {} の中です",
                out.display(),
                tree.display()
            );
        }
    }
    // 木を読む前に止める。書き出しの最後でも create_new で同じことを確かめる。
    ensure!(
        std::fs::symlink_metadata(&args.conflicts).is_err(),
        "{} は既にあります (--conflicts は新しいファイルにしてください)",
        args.conflicts.display()
    );
    let schema = Schema::load(args.schema.as_deref())?;
    let skip_keys = args.skip_keys.as_deref().map(read_skip_keys).transpose()?.unwrap_or_default();
    let mut unknown = 0;
    let mut nodes = BTreeMap::new();
    let mut disagreements = 0;
    let mut skipped = BTreeSet::new();
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
        let tree_readings = tree_readings(dir)?;
        let known_nodes = tree_readings.iter().map(|r| r.key.clone()).collect();
        let mut parsed =
            parse_node_moves(&text, tree, (!tree_readings.is_empty()).then_some(&known_nodes))?;
        readings.push(if fingerprint.is_none() && !args.journal.is_empty() {
            tree_readings
        } else {
            Vec::new()
        });
        fingerprints.push(fingerprint);
        skipped.extend(skip_nodes(&mut parsed, &skip_keys));
        disagreements += merge(&mut nodes, parsed);
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
    let mut rows = parse_master(&text)?;
    for row in &rows {
        schema.validate_row(row)?;
    }
    let before = rows.len();
    let unpromoted = super::unpromotion::demote_rows(&mut rows, &args.since)?;
    validate(&rows)?;
    let unpromoted_moves = super::unpromotion::drop_node_moves(&mut nodes)?;
    eprintln!("{} / 木の手から外した不成 {unpromoted_moves}", unpromoted.format());
    let options = Options {
        since: &args.since,
        note: &args.note,
        alt_gap: args.alt_gap,
        source: "read",
        lineage: "",
        schema: &schema,
        replace_curated: args.replace_curated,
        revive_bench: true,
        kept: None,
    };
    let missing = skip_keys.iter().filter(|key| !contains_orientation(&skipped, key)).count();
    let skipped = skipped.len();
    // 登録後の元データで比べるのは、続く master audit が見る値と揃え、木が元の main の先を読み直した分も入れるため。
    // main と alt の入れ替えは伝播の値を変えないので、仮の登録の値で残す局面を決めて登録し直せばよい。
    let (tentative, _, _) = register(rows.clone(), &nodes, &options)?;
    let rule = keep::KeepRule {
        rule: args.switch_rule,
        line: keep_line,
        min_depth: args.keep_min_depth.unwrap_or(1),
        draws: DrawValues {
            black: args.draw_value_black,
            white: args.draw_value_white,
        },
        fill: if args.bridge_replies {
            Fill::Bridge
        } else {
            Fill::Off
        },
    };
    let (kept, mut singles) = keep::kept_mains(&rows, &tentative, &nodes, rule)?;
    drop(tentative);
    let single = match &args.single.single_enqueue_out {
        Some(_) => {
            switch::rank(&mut singles, None, 0.0);
            let (lines, excluded) = switch::queue(&singles, &args.single)?;
            Some((super::refutation::format_queue(&lines)?, lines.len(), excluded))
        }
        None => None,
    };
    let options = Options {
        kept: Some(&kept),
        ..options
    };
    let (rows, conflicts, summary) = register(rows, &nodes, &options)?;
    let mut table = String::from(
        "key\tply\tside\tmain\tmain_source\tmain_tier\tmain_tree_value\ttree_best\ttree_value\ttree\treason\tmain_value\tmain_depth\tbest_value\tbest_depth\tgap\tmain_value_black\tbest_value_black\n",
    );
    for c in &conflicts {
        write!(table, "{c}\t{}\t", args.trees[c.tree].display())?;
        match &c.kept {
            Some(k) => {
                let black = |v: i32| {
                    if c.key.split(' ').nth(1) == Some("w") {
                        -v
                    } else {
                        v
                    }
                };
                writeln!(
                    table,
                    "keep\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    k.old_value,
                    k.old_depth,
                    k.new_value,
                    k.new_depth,
                    k.gap(),
                    black(k.old_value),
                    black(k.new_value)
                )?;
            }
            None => writeln!(table, "protected\t-\t-\t-\t-\t-\t-\t-")?,
        }
    }
    write_new(&args.conflicts, &table)?;
    if let (Some(path), Some((text, queued, excluded))) = (&args.single.single_enqueue_out, single)
    {
        write_atomic(path, &text)?;
        println!(
            "1 回の読みの手を伸ばす木のキューの行: {queued} (既存の木で除いた局面 {excluded})"
        );
    }
    for side in ["b", "w"] {
        println!(
            "{side}: main を足した局面 {} / alt を足した手 {}",
            summary.added_main.get(side).unwrap_or(&0),
            summary.added_alt.get(side).unwrap_or(&0)
        );
    }
    println!(
        "main を木の 1 位に替えた局面 {} (うち curated の main {}) / 評価値を伝播後の値にした行 {} / bench から戻した手 {} / 保護された控え {} / curated の main が無い控え {} / record なので足さなかった手 {} / 食い違い {} (うち元の main の値が上なので残した局面 {}) ({}) / 木どうしで 1 位が違った局面 {disagreements}",
        summary.replaced_main,
        summary.replaced_curated,
        summary.evals_updated,
        summary.bench_revived,
        summary.bench_protected,
        summary.bench_curated_without_main,
        summary.record_skipped,
        summary.conflicts,
        kept.len(),
        args.conflicts.display()
    );
    println!("cond=unknown の木: {unknown}");
    println!("指定で取り込まなかった局面 {skipped} / 指定したが木に無かった局面 {missing}");
    if args.apply {
        write_atomic(&args.master, &format_master(&rows))?;
        println!("書き換えました: 行 {before} -> {}", rows.len());
    } else {
        println!("確認だけ (--apply なし): 行 {before} -> {}", rows.len());
    }
    Ok(())
}

fn read_skip_keys(path: &Path) -> Result<BTreeSet<String>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("{} を読めません", path.display()))?;
    let mut keys = BTreeSet::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line =
            line.with_context(|| format!("{}: {} 行目を読めません", path.display(), i + 1))?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parsed = || -> Result<String> {
            let parts: Vec<_> = line.split_whitespace().collect();
            ensure!(matches!(parts.len(), 3 | 4), "SFEN は 3 または 4 フィールドにしてください");
            let key = parts[..3].join(" ");
            let sfen = format!("{key} {}", parts.get(3).unwrap_or(&"1"));
            let position = position_from_sfen(&sfen)?;
            Ok(strip_ply(&position.to_sfen()).to_owned())
        };
        keys.insert(parsed().with_context(|| {
            format!("{}: {} 行目の局面キーが不正です: {line}", path.display(), i + 1)
        })?);
    }
    Ok(keys)
}

fn contains_orientation(keys: &BTreeSet<String>, key: &str) -> bool {
    keys.contains(key)
        || rshogi_book::flipped_key(&format!("{key} 1"))
            .is_some_and(|flipped| keys.contains(strip_ply(&flipped)))
}

fn skip_nodes(nodes: &mut BTreeMap<String, Node>, keys: &BTreeSet<String>) -> BTreeSet<String> {
    let mut skipped = BTreeSet::new();
    for key in keys {
        if nodes.remove(key).is_some() {
            skipped.insert(key.clone());
        }
        if let Some(flipped) = rshogi_book::flipped_key(&format!("{key} 1")) {
            let flipped = strip_ply(&flipped);
            if nodes.remove(flipped).is_some() {
                skipped.insert(flipped.to_owned());
            }
        }
    }
    skipped
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

pub(super) fn parse_node_moves(
    text: &str,
    tree: usize,
    known_nodes: Option<&BTreeSet<String>>,
) -> Result<BTreeMap<String, Node>> {
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
            value_kind: if cells[7] == "1" {
                "unknown".into()
            } else {
                "single".into()
            },
        });
    }
    let remaining: BTreeMap<_, _> =
        nodes.iter().map(|(key, node)| (key.clone(), node.remaining)).collect();
    for (key, node) in &mut nodes {
        for mv in &mut node.moves {
            let destination = child(key, &mv.mv)?;
            if let Some(depth) = remaining.get(&destination) {
                let depth = depth.checked_add(1).context("行き先の remaining が大きすぎます")?;
                mv.value_kind = format!("tree:{depth}");
            } else if known_nodes.is_none_or(|known| known.contains(&destination)) {
                // TSV にない局面への合流や、読みの一覧がない木では、伝播の有無や深さを確定できない。
                mv.value_kind = "unknown".into();
            }
        }
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
    /// 元の main の伝播後の値が上なので残したときの値。`None` は source・curated で替えなかった局面。
    pub kept: Option<Kept>,
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
        let main = existing.iter().copied().find(|&i| rows[i].get("role") == "main");
        let curated_main = main.is_some_and(|i| rows[i].get("tier") == "curated");
        let playable: Vec<&NodeMove> = node
            .moves
            .iter()
            .filter(|m| {
                let record = find(&rows, &m.mv).is_some_and(|i| rows[i].get("role") == "record");
                if record {
                    summary.record_skipped += 1;
                }
                let bench = find(&rows, &m.mv).filter(|&i| rows[i].get("role") == "bench");
                let protected =
                    bench.is_some_and(|i| !options.schema.replaceable(rows[i].get("source")));
                summary.bench_protected += usize::from(protected);
                let curated_without_main =
                    bench.is_some_and(|i| rows[i].get("tier") == "curated" && !curated_main);
                summary.bench_curated_without_main += usize::from(curated_without_main);
                !record
                    && !protected
                    && !curated_without_main
                    && (options.revive_bench || bench.is_none())
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
        let best_was_bench = find(&rows, &best.mv).is_some_and(|i| rows[i].get("role") == "bench");
        for candidate in std::iter::once(best).chain(alts.iter().copied()) {
            if let Some(i) = find(&rows, &candidate.mv)
                && rows[i].get("role") == "bench"
            {
                let role = if main.is_none() && candidate.mv == best.mv {
                    "main"
                } else {
                    "alt"
                };
                rows[i].set("role", role.into());
                let note = append(
                    rows[i].get("note"),
                    format!("{}: 値が戻ったので bench から {role}", options.since),
                );
                rows[i].set("note", note);
                summary.bench_revived += 1;
            }
        }
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
        let curated = existing.iter().any(|&i| rows[i].get("tier") == "curated");
        match main {
            None => {
                if let Some(i) = find(&rows, &best.mv) {
                    ensure!(
                        best_was_bench,
                        "{key}: main の無い局面に {} の行があります",
                        rows[i].get("move")
                    );
                    rows[i].set("role", "main".into());
                } else {
                    rows.push(new_row(best, "main", "木の伝播後の 1 位".into()));
                    *summary.added_main.entry(side.clone()).or_default() += 1;
                }
            }
            Some(m) if rows[m].get("move") == best.mv => {}
            Some(m)
                if options.schema.replaceable(rows[m].get("source"))
                    && (options.replace_curated || (rows[m].get("tier") == "wide" && !curated))
                    && !options.kept.is_some_and(|kept| kept.contains_key(key)) =>
            {
                // curated の main を替えるときは新しい main を curated にし、この局面の curated の main を 1 つに保つ。
                let tier = rows[m].get("tier").to_owned();
                rows[m].set("role", "alt".into());
                rows[m].set("tier", "wide".into());
                let demoted = if tier == "curated" {
                    summary.replaced_curated += 1;
                    " (curated から wide)"
                } else {
                    ""
                };
                let note = append(
                    rows[m].get("note"),
                    format!(
                        "{}: 木の伝播後の 1 位でないので main から alt{demoted}",
                        options.since
                    ),
                );
                rows[m].set("note", note);
                match find(&rows, &best.mv) {
                    Some(i) => {
                        rows[i].set("role", "main".into());
                        rows[i].set("tier", tier.clone());
                        let note = append(
                            rows[i].get("note"),
                            format!("{}: 木の伝播後の 1 位なので alt から main", options.since),
                        );
                        rows[i].set("note", note);
                    }
                    None => {
                        let mut row = new_row(best, "main", "木の伝播後の 1 位".into());
                        row.set("tier", tier);
                        rows.push(row);
                    }
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
                    kept: options.kept.and_then(|kept| kept.get(key)).cloned(),
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
            if row.get("role") == "record"
                || (row.get("role") == "bench" && !options.revive_bench)
                || !options.schema.replaceable(row.get("source"))
            {
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
            replace_curated: false,
            revive_bench: true,
            kept: None,
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
    fn skip_keys_preserve_existing_rows_and_omit_new_positions() {
        let b = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let w = child(b, "5i4i").unwrap();
        let added = child(&w, "5a4a").unwrap();
        let absent = child(&added, "4i3i").unwrap();
        let original = master(&[
            row(b, "5i6i", "main", "read", "curated", "1,2"),
            row(b, "5i4i", "alt", "read", "wide", "3"),
            row(b, "5i5h", "bench", "read", "wide", "4"),
        ]);
        let mut nodes = BTreeMap::from([
            (b.into(), node(&[("5i4i", 50), ("5i5h", 49), ("5i6h", 48)], 2, 0)),
            (w.clone(), node(&[("5a4a", 10)], 1, 0)),
            (added.clone(), node(&[("4i3i", 20), ("4i4h", 19)], 0, 0)),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skip.txt");
        std::fs::write(&path, format!("# 除外\n\n {b} 17 \n{w}\n{b}\n{absent}\n")).unwrap();
        let keys = read_skip_keys(&path).unwrap();
        assert_eq!(keys.len(), 3);
        let skipped = skip_nodes(&mut nodes, &keys);
        assert_eq!(skipped.len(), 2);
        assert_eq!(keys.iter().filter(|key| !contains_orientation(&skipped, key)).count(), 1);
        let options = Options {
            replace_curated: true,
            ..options()
        };
        let (rows, conflicts, summary) = register(original.clone(), &nodes, &options).unwrap();
        let preserved: Vec<_> = rows.iter().filter(|r| r.get("key") == b).cloned().collect();
        assert_eq!(format_master(&preserved), format_master(&original));
        assert!(!rows.iter().any(|r| r.get("key") == w));
        assert_eq!(find(&rows, &added, "4i3i").unwrap().get("role"), "main");
        assert_eq!(find(&rows, &added, "4i4h").unwrap().get("role"), "alt");
        assert_eq!(summary.added_main["b"], 1);
        assert_eq!(summary.added_alt["b"], 1);
        assert_eq!(summary.evals_updated, 0);
        assert_eq!(summary.replaced_main, 0);
        assert_eq!(summary.bench_revived, 0);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn skip_keys_normalize_ply_and_hand_order() {
        let key = "4k4/9/9/9/9/9/9/9/4K4 b 2BP";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skip.txt");
        for input in [
            format!("{key} 17"),
            format!("{key} +1"),
            format!("{key} -0"),
            key.replace("2BP", "P2B"),
            format!("{} +1", key.replace("2BP", "P2B")),
        ] {
            std::fs::write(&path, &input).unwrap();
            let keys = read_skip_keys(&path).unwrap();
            assert_eq!(keys, BTreeSet::from([key.to_owned()]), "{input}");
            let mut nodes = BTreeMap::from([(key.into(), node(&[("5i4i", 90)], 0, 0))]);
            assert_eq!(skip_nodes(&mut nodes, &keys).len(), 1, "{input}");
            let original = master(&[row(key, "5i4i", "main", "read", "wide", "3")]);
            let (rows, _, summary) = register(original.clone(), &nodes, &options()).unwrap();
            assert_eq!(format_master(&rows), format_master(&original), "{input}");
            assert_eq!(summary.evals_updated, 0);
        }
    }

    #[test]
    fn skipped_positions_do_not_count_as_tree_disagreements() {
        let key = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let keys = BTreeSet::from([key.to_owned()]);
        let mut merged = BTreeMap::new();
        let mut skipped = BTreeSet::new();
        let mut disagreements = 0;
        for (tree, mv) in ["5i4i", "5i6i"].into_iter().enumerate() {
            let mut nodes = BTreeMap::from([(key.into(), node(&[(mv, 10)], 0, tree))]);
            skipped.extend(skip_nodes(&mut nodes, &keys));
            disagreements += merge(&mut merged, nodes);
        }
        assert_eq!(skipped.len(), 1);
        assert_eq!(disagreements, 0);
    }

    #[test]
    fn skip_keys_match_both_orientations_and_reject_invalid_sfen() {
        let b = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let flipped = rshogi_book::flipped_key(&format!("{b} 1")).unwrap();
        let flipped = strip_ply(&flipped);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skip.txt");
        std::fs::write(&path, format!("{b}\n")).unwrap();
        let mut nodes = BTreeMap::from([
            (b.into(), node(&[("5i4i", 10)], 0, 0)),
            (flipped.into(), node(&[("5a4a", 10)], 0, 0)),
        ]);
        let keys = read_skip_keys(&path).unwrap();
        assert_eq!(skip_nodes(&mut nodes, &keys).len(), 2);
        assert!(nodes.is_empty());
        nodes.insert(flipped.into(), node(&[("5a4a", 10)], 0, 0));
        let skipped = skip_nodes(&mut nodes, &keys);
        assert_eq!(skipped.len(), 1);
        assert!(contains_orientation(&skipped, b));
        assert!(nodes.is_empty());
        for invalid in [
            "pos1 b -".to_owned(),
            b.replace(" b ", " x "),
            b.replace(" b -", " b X"),
            format!("{b} bad"),
            format!("{b} 1 extra"),
            "4k4/9/9/9/9/9/9/9/4K4 b".to_owned(),
        ] {
            std::fs::write(&path, format!("# 除外\n{invalid}\n")).unwrap();
            let error = read_skip_keys(&path).unwrap_err();
            assert!(error.to_string().contains("2 行目の局面キーが不正"), "{error}");
        }
    }

    #[test]
    fn read_main_is_replaced_but_owner_and_curated_mains_become_conflicts() {
        let k3 = "pos3 b -";
        let rows = master(&[
            row(B, "old", "main", "read", "wide", "1,2"),
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
    fn replace_curated_moves_curated_mains_but_keeps_protected_sources() {
        let k3 = "pos3 b -";
        let rows = master(&[
            row(B, "careful", "main", "read", "curated", "3"),
            row(B, "side", "alt", "read", "curated", "1"),
            row(W, "plugged", "main", "plug", "curated", "9"),
            row(k3, "old", "main", "read", "wide", "1"),
            row(k3, "never", "record", "read", "curated", "-30000"),
        ]);
        let nodes = BTreeMap::from([
            (B.into(), node(&[("better", 60), ("careful", 0)], 1, 0)),
            (W.into(), node(&[("tree", 60), ("plugged", 0)], 1, 0)),
            (k3.into(), node(&[("new", 30), ("old", -20)], 1, 0)),
        ]);
        let options = Options {
            replace_curated: true,
            ..options()
        };
        let (rows, conflicts, summary) = register(rows, &nodes, &options).unwrap();
        let get = |key, mv| {
            let r = find(&rows, key, mv).unwrap();
            (r.get("role").to_owned(), r.get("tier").to_owned())
        };
        assert_eq!(get(B, "better"), ("main".into(), "curated".into()));
        assert_eq!(get(B, "careful"), ("alt".into(), "wide".into()));
        assert_eq!(get(B, "side"), ("alt".into(), "curated".into()));
        assert_eq!(get(W, "plugged"), ("main".into(), "curated".into()));
        assert_eq!(get(k3, "new"), ("main".into(), "wide".into()));
        assert_eq!(get(k3, "never"), ("record".into(), "curated".into()));
        assert_eq!((summary.replaced_main, summary.replaced_curated, conflicts.len()), (2, 1, 1));
        assert!(find(&rows, B, "careful").unwrap().get("note").ends_with("(curated から wide)"));
        assert_eq!(conflicts[0].main, "plugged");
    }

    #[test]
    fn replace_curated_promotes_an_existing_alt_with_the_main_tier() {
        let rows = master(&[
            row(B, "careful", "main", "read", "curated", "3"),
            row(B, "better", "alt", "read", "wide", "1"),
        ]);
        let nodes = BTreeMap::from([(B.into(), node(&[("better", 60), ("careful", 0)], 1, 0))]);
        let options = Options {
            replace_curated: true,
            ..options()
        };
        let (rows, conflicts, summary) = register(rows, &nodes, &options).unwrap();
        let better = find(&rows, B, "better").unwrap();
        assert_eq!((better.get("role"), better.get("tier")), ("main", "curated"));
        let careful = find(&rows, B, "careful").unwrap();
        assert_eq!((careful.get("role"), careful.get("tier")), ("alt", "wide"));
        assert_eq!((summary.replaced_main, conflicts.len()), (1, 0));
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
        const B: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let white = child(B, "5i4i").unwrap();
        let w = white.as_str();
        let dir = tempfile::tempdir().unwrap();
        let master_path = dir.path().join("moves.tsv");
        let original = format_master(&master(&[row(B, "pinned", "main", "pin", "wide", "")]));
        std::fs::write(&master_path, &original).unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(tree.join("STATUS"), "DONE\n").unwrap();
        std::fs::write(
            tree.join(NODE_MOVES),
            format!("key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n{B}\t3\t0\t2\t1000000000\t5i4i\t40\t1\t1\n{w}\t4\t1\t1\t1000000000\t5a4a\t-5\t1\t1\n"),
        )
        .unwrap();
        let args = |trees: Vec<PathBuf>, apply| RegisterArgs {
            master: master_path.clone(),
            schema: None,
            journal: Vec::new(),
            since: "tag".into(),
            note: String::new(),
            alt_gap: 40,
            replace_curated: false,
            skip_keys: None,
            conflicts: dir.path().join("conflicts.tsv"),
            apply,
            switch_rule: SwitchRule::Line,
            single: switch::test_args(),
            keep_line: None,
            keep_min_depth: None,
            bridge_replies: false,
            draw_value_black: -70,
            draw_value_white: 0,
            trees,
        };
        run(args(vec![tree.clone()], false)).unwrap();
        assert_eq!(std::fs::read_to_string(&master_path).unwrap(), original);
        let conflicts = std::fs::read_to_string(dir.path().join("conflicts.tsv")).unwrap();
        assert_eq!(conflicts.lines().count(), 2);
        let line = conflicts.lines().nth(1).unwrap();
        assert!(line.starts_with(&format!("{B}\t3\tb\tpinned\tpin\twide\t-\t5i4i\t40\t")));
        assert!(line.ends_with("\tprotected\t-\t-\t-\t-\t-\t-\t-"));
        assert_eq!(conflicts.lines().next().unwrap().split('\t').count(), line.split('\t').count());
        assert!(run(args(vec![tree.clone()], true)).is_err());
        assert_eq!(std::fs::read_to_string(&master_path).unwrap(), original);
        std::fs::remove_file(dir.path().join("conflicts.tsv")).unwrap();
        run(args(vec![tree.clone()], true)).unwrap();
        let rows = parse_master(&std::fs::read_to_string(&master_path).unwrap()).unwrap();
        assert_eq!(find(&rows, w, "5a4a").unwrap().get("role"), "main");
        assert_eq!(
            find(&rows, B, "5i4i").unwrap().get("note"),
            "木の伝播後の 1 位 (main と食い違い)"
        );

        let skipped_original =
            format_master(&master(&[row(B, "5i4i", "main", "read", "wide", "7")]));
        std::fs::write(&master_path, &skipped_original).unwrap();
        let skip_path = dir.path().join("skip.txt");
        std::fs::write(&skip_path, format!("{B}\n")).unwrap();
        let mut skipping = args(vec![tree.clone()], true);
        skipping.skip_keys = Some(skip_path);
        skipping.conflicts = dir.path().join("skipped-conflicts.tsv");
        run(skipping).unwrap();
        let rows = parse_master(&std::fs::read_to_string(&master_path).unwrap()).unwrap();
        assert_eq!(find(&rows, B, "5i4i").unwrap().get("evals"), "7");
        let preserved: Vec<_> = rows.iter().filter(|row| row.get("key") == B).cloned().collect();
        assert_eq!(format_master(&preserved), skipped_original);
        assert_eq!(find(&rows, w, "5a4a").unwrap().get("role"), "main");

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
    fn run_demotes_needless_unpromotions_and_does_not_register_them_from_trees() {
        const W: &str = "4k4/7b1/9/9/9/9/9/9/4K4 w -";
        let dir = tempfile::tempdir().unwrap();
        let master_path = dir.path().join("moves.tsv");
        std::fs::write(
            &master_path,
            format_master(&master(&[
                row(W, "2b7g", "main", "ext", "wide", "50"),
                row(W, "2b3c", "alt", "ext", "wide", "45"),
                row(W, "2b6f", "alt", "ext", "wide", "20"),
            ])),
        )
        .unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(tree.join("STATUS"), "DONE\n").unwrap();
        std::fs::write(
            tree.join(NODE_MOVES),
            format!("key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n{W}\t2\t0\t0\t1000000000\t2b8h\t90\t0\t1\n{W}\t2\t0\t0\t1000000000\t2b6f\t35\t0\t2\n"),
        )
        .unwrap();
        run(RegisterArgs {
            master: master_path.clone(),
            schema: None,
            journal: Vec::new(),
            since: "tag".into(),
            note: String::new(),
            alt_gap: 40,
            replace_curated: false,
            skip_keys: None,
            conflicts: dir.path().join("conflicts.tsv"),
            apply: true,
            switch_rule: SwitchRule::Line,
            single: switch::test_args(),
            keep_line: None,
            keep_min_depth: None,
            bridge_replies: false,
            draw_value_black: -70,
            draw_value_white: 0,
            trees: vec![tree],
        })
        .unwrap();
        let rows = parse_master(&std::fs::read_to_string(&master_path).unwrap()).unwrap();
        validate(&rows).unwrap();
        assert_eq!(find(&rows, W, "2b7g").unwrap().get("role"), "record");
        assert!(find(&rows, W, "2b8h").is_none());
        let main: Vec<_> =
            rows.iter().filter(|r| r.get("role") == "main").map(|r| r.get("move")).collect();
        assert_eq!(main, ["2b6f"]);
    }

    #[test]
    fn main_backed_by_a_deeper_reply_is_kept_when_the_tree_best_is_worse() {
        const R: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let a = child(R, "5i4i").unwrap();
        let before = master(&[
            row(R, "5i4i", "main", "read", "wide", "10"),
            row(&a, "5a4a", "main", "read", "wide", "-50"),
        ]);
        let tree = |moves: &[(&str, i32)]| BTreeMap::from([(R.to_owned(), node(moves, 1, 0))]);
        let rule = |line, min_depth| keep::KeepRule {
            rule: SwitchRule::Line,
            line,
            min_depth,
            draws: DrawValues::both(0),
            fill: Fill::Off,
        };
        let judge = |nodes: &BTreeMap<String, Node>, rule| {
            let (after, _, _) = register(before.clone(), nodes, &options()).unwrap();
            keep::kept_mains(&before, &after, nodes, rule).unwrap().0
        };
        // 木は 5i4i を 1 回の読みで 5 としたが、元データでは応手 5a4a の先から 50 が伝わる。
        let nodes = tree(&[("5i6i", 30), ("5i4i", 5)]);
        let kept = judge(&nodes, rule(20, 1));
        let expected = Kept {
            old_value: 50,
            old_depth: 1,
            new_value: 30,
            new_depth: 0,
        };
        assert_eq!(kept, BTreeMap::from([(R.to_owned(), expected.clone())]));
        assert!(judge(&nodes, rule(21, 1)).is_empty());
        assert!(judge(&nodes, rule(20, 2)).is_empty());
        assert!(judge(&nodes, rule(0, 0)).is_empty());

        let keeping = Options {
            kept: Some(&kept),
            ..options()
        };
        let (rows, conflicts, summary) = register(before.clone(), &nodes, &keeping).unwrap();
        assert_eq!(find(&rows, R, "5i4i").unwrap().get("role"), "main");
        assert_eq!(find(&rows, R, "5i6i").unwrap().get("role"), "alt");
        assert_eq!((summary.replaced_main, summary.conflicts), (0, 1));
        assert_eq!(conflicts[0].kept, Some(expected));

        // 木の読みに無い元の main の値が 1 回の読みだけなら、--keep-min-depth 1 では残さない。
        let single = master(&[row(R, "5i4i", "main", "read", "wide", "60")]);
        let nodes = tree(&[("5i6i", 30)]);
        let (after, _, _) = register(single.clone(), &nodes, &options()).unwrap();
        assert!(keep::kept_mains(&single, &after, &nodes, rule(20, 1)).unwrap().0.is_empty());
        let kept = keep::kept_mains(&single, &after, &nodes, rule(20, 0)).unwrap().0;
        assert_eq!((kept[R].gap(), kept[R].old_depth), (30, 0));
    }

    #[test]
    fn propagated_rule_switches_only_when_both_values_come_from_children() {
        const R: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let a = child(R, "5i4i").unwrap();
        let b = child(R, "5i6i").unwrap();
        let rule = keep::KeepRule {
            rule: SwitchRule::Propagated,
            line: 20,
            min_depth: 1,
            draws: DrawValues::both(0),
            fill: Fill::Off,
        };
        let nodes = BTreeMap::from([(R.to_owned(), node(&[("5i6i", 30), ("5i4i", 5)], 1, 0))]);
        let judge = |rows: &[String]| {
            let before = master(rows);
            let (after, _, _) = register(before.clone(), &nodes, &options()).unwrap();
            keep::kept_mains(&before, &after, &nodes, rule).unwrap()
        };
        let main = row(R, "5i4i", "main", "read", "wide", "10");
        let old = row(&a, "5a4a", "main", "read", "wide", "-50");
        // 木の 1 位 5i6i の先が元データにあり 51 なら替え、同点なら残す。
        for (value, switched) in [("-51", true), ("-50", false)] {
            let (kept, singles) = judge(&[
                main.clone(),
                old.clone(),
                row(&b, "5a4a", "main", "read", "wide", value),
            ]);
            assert_eq!(kept.is_empty(), switched, "{value}");
            assert!(singles.is_empty());
        }
        // 5i6i の値が木の 1 回の読みだけなら、替えずに 5i6i を伸ばす行を出す。
        let (kept, singles) = judge(&[main.clone(), old.clone()]);
        assert_eq!((kept[R].old_depth, kept[R].new_depth), (1, 0));
        assert_eq!(singles.len(), 1);
        assert_eq!(
            (singles[0].moves.clone(), singles[0].ply.as_str()),
            (vec!["5i6i".to_owned()], "12")
        );
        // 元の main の値が 1 回の読みでも替えない。
        let (kept, singles) =
            judge(&[main.clone(), row(&b, "5a4a", "main", "read", "wide", "-60")]);
        assert!(kept.contains_key(R));
        assert_eq!(singles[0].moves, vec!["5i4i".to_owned()]);
    }

    #[test]
    fn run_lists_kept_mains_with_values_from_both_sides() {
        const W: &str = "4k4/9/9/9/9/9/9/9/4K4 w -";
        let a = child(W, "5a4a").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let master_path = dir.path().join("moves.tsv");
        let original = format_master(&master(&[
            row(W, "5a4a", "main", "read", "wide", "10"),
            row(&a, "5i4i", "main", "read", "wide", "-50"),
        ]));
        std::fs::write(&master_path, &original).unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(tree.join("STATUS"), "DONE\n").unwrap();
        std::fs::write(
            tree.join(NODE_MOVES),
            format!("key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n{W}\t4\t0\t0\t1000000000\t5a6a\t30\t0\t1\n{W}\t4\t0\t0\t1000000000\t5a4a\t5\t0\t2\n"),
        )
        .unwrap();
        run(RegisterArgs {
            master: master_path.clone(),
            schema: None,
            journal: Vec::new(),
            since: "tag".into(),
            note: String::new(),
            alt_gap: 40,
            replace_curated: false,
            skip_keys: None,
            conflicts: dir.path().join("conflicts.tsv"),
            apply: true,
            switch_rule: SwitchRule::Line,
            single: switch::test_args(),
            keep_line: None,
            keep_min_depth: None,
            bridge_replies: false,
            draw_value_black: -70,
            draw_value_white: 0,
            trees: vec![tree],
        })
        .unwrap();
        let rows = parse_master(&std::fs::read_to_string(&master_path).unwrap()).unwrap();
        assert_eq!(find(&rows, W, "5a4a").unwrap().get("role"), "main");
        assert_eq!(find(&rows, W, "5a6a").unwrap().get("role"), "alt");
        let conflicts = std::fs::read_to_string(dir.path().join("conflicts.tsv")).unwrap();
        let line = conflicts.lines().nth(1).unwrap();
        assert!(line.starts_with(&format!("{W}\t4\tw\t5a4a\tread\twide\t5\t5a6a\t30\t")));
        assert!(line.ends_with("\tkeep\t50\t1\t30\t0\t20\t-50\t-30"), "{line}");
    }

    #[test]
    fn run_with_propagated_rule_writes_single_read_queue() {
        const W: &str = "4k4/9/9/9/9/9/9/9/4K4 w -";
        let a = child(W, "5a4a").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let master_path = dir.path().join("moves.tsv");
        std::fs::write(
            &master_path,
            format_master(&master(&[
                row(W, "5a4a", "main", "read", "wide", "10"),
                row(&a, "5i4i", "main", "read", "wide", "-50"),
            ])),
        )
        .unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(tree.join("STATUS"), "DONE\n").unwrap();
        std::fs::write(
            tree.join(NODE_MOVES),
            format!("key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n{W}\t4\t0\t0\t1000000000\t5a6a\t60\t0\t1\n{W}\t4\t0\t0\t1000000000\t5a4a\t5\t0\t2\n"),
        )
        .unwrap();
        let queue = dir.path().join("single.jsonl");
        let args = |keep_line| RegisterArgs {
            master: master_path.clone(),
            schema: None,
            journal: Vec::new(),
            since: "tag".into(),
            note: String::new(),
            alt_gap: 40,
            replace_curated: false,
            skip_keys: None,
            conflicts: dir.path().join("conflicts.tsv"),
            apply: true,
            switch_rule: SwitchRule::Propagated,
            single: SingleQueueArgs {
                single_enqueue_out: Some(queue.clone()),
                ..switch::test_args()
            },
            keep_line,
            keep_min_depth: None,
            bridge_replies: false,
            draw_value_black: -70,
            draw_value_white: 0,
            trees: vec![tree.clone()],
        };
        assert!(run(args(Some(20))).is_err());
        let node_moves = std::fs::read(tree.join(NODE_MOVES)).unwrap();
        let mut inside = args(None);
        inside.single.single_enqueue_out = Some(tree.join(NODE_MOVES));
        assert!(run(inside).is_err());
        assert_eq!(std::fs::read(tree.join(NODE_MOVES)).unwrap(), node_moves);
        run(args(None)).unwrap();
        let rows = parse_master(&std::fs::read_to_string(&master_path).unwrap()).unwrap();
        assert_eq!(find(&rows, W, "5a4a").unwrap().get("role"), "main");
        assert_eq!(find(&rows, W, "5a6a").unwrap().get("role"), "alt");
        let conflicts = std::fs::read_to_string(dir.path().join("conflicts.tsv")).unwrap();
        assert!(conflicts.lines().nth(1).unwrap().contains("\tkeep\t50\t1\t60\t0\t"));
        let lines: Vec<crate::book_tree::targets::Line> = std::fs::read_to_string(&queue)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].root, format!("sfen {W} 4"));
        assert_eq!(lines[0].own, "w");
        assert_eq!(lines[0].game, vec![vec!["5a6a".to_owned()]]);
    }

    #[test]
    fn bench_source_and_caller_control_revival_without_main() {
        for has_main in [false, true] {
            for enabled in [false, true] {
                for protected in [false, true] {
                    let dir = tempfile::tempdir().unwrap();
                    crate::book_tree::schema::test_schema(dir.path());
                    std::fs::write(
                        dir.path().join("sources.tsv"),
                        format!(
                            "source\tdescription\treplaceable\nread\t読み\t1\ncustom\t控え\t{}\n",
                            usize::from(!protected)
                        ),
                    )
                    .unwrap();
                    let schema = Schema::load(Some(dir.path())).unwrap();
                    let mut input = vec![row(B, "best", "bench", "custom", "wide", "-999")];
                    if has_main {
                        input.push(row(B, "old", "main", "read", "wide", "0"));
                    }
                    let rows = master(&input);
                    let original = find(&rows, B, "best").unwrap().clone();
                    let nodes =
                        BTreeMap::from([(B.into(), node(&[("best", 100), ("old", 0)], 1, 0))]);
                    let (rows, _, summary) = register(
                        rows,
                        &nodes,
                        &Options {
                            schema: &schema,
                            revive_bench: enabled,
                            ..options()
                        },
                    )
                    .unwrap();
                    let best = find(&rows, B, "best").unwrap();
                    assert_eq!(
                        best.get("role"),
                        if enabled && !protected {
                            "main"
                        } else {
                            "bench"
                        }
                    );
                    if !has_main && enabled && !protected {
                        assert!(best.get("note").ends_with("bench から main"));
                        assert!(summary.added_main.is_empty());
                    }
                    if protected {
                        assert_eq!(best, &original);
                    }
                    assert_eq!(summary.bench_protected, usize::from(protected));
                    assert_eq!(summary.bench_revived, usize::from(enabled && !protected));
                    validate(&rows).unwrap();
                }
            }
        }
    }

    #[test]
    fn curated_bench_without_curated_main_is_excluded_from_candidates() {
        for has_main in [false, true] {
            for replace_curated in [false, true] {
                let mut input = vec![row(B, "bench", "bench", "read", "curated", "-999")];
                if has_main {
                    input.push(row(B, "old", "main", "read", "wide", "0"));
                }
                let moves = if has_main {
                    vec![("bench", 100), ("old", 0)]
                } else {
                    vec![("new", 100), ("bench", 99)]
                };
                let nodes = BTreeMap::from([(B.into(), node(&moves, 1, 0))]);
                let (rows, _, summary) = register(
                    master(&input),
                    &nodes,
                    &Options {
                        replace_curated,
                        ..options()
                    },
                )
                .unwrap();
                assert_eq!(find(&rows, B, "bench").unwrap().get("role"), "bench");
                assert_eq!(summary.bench_curated_without_main, 1);
                assert_eq!(summary.bench_revived, 0);
                assert_eq!(
                    find(&rows, B, if has_main { "old" } else { "new" }).unwrap().get("role"),
                    "main"
                );
                validate(&rows).unwrap();
            }
        }
    }

    #[test]
    fn bench_best_and_close_alts_return() {
        for source in ["read", "protected"] {
            let rows = master(&[
                row(B, "old", "main", source, "wide", "0"),
                row(B, "best", "bench", "read", "wide", "-999"),
                row(B, "close", "bench", "read", "wide", "-999"),
                row(B, "bad", "bench", "read", "wide", "-999"),
            ]);
            let nodes = BTreeMap::from([(
                B.into(),
                node(&[("best", 100), ("close", 99), ("old", 0), ("bad", -999)], 1, 0),
            )]);
            let (rows, _, summary) = register(rows, &nodes, &options()).unwrap();
            assert_eq!(
                find(&rows, B, "best").unwrap().get("role"),
                if source == "read" { "main" } else { "alt" }
            );
            assert_eq!(find(&rows, B, "best").unwrap().get("evals"), "100");
            assert_eq!(find(&rows, B, "close").unwrap().get("role"), "alt");
            assert_eq!(find(&rows, B, "bad").unwrap().get("role"), "bench");
            assert_eq!(summary.bench_revived, 2);
        }
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
        const B: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let white = child(B, "5i4i").unwrap();
        let w = white.as_str();
        let table = format!(
            "key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n{B}\t1\t0\t2\t100\t5i4i\t10\t1\t1\n{B}\t1\t0\t2\t100\t5i6i\t9\t0\t2\n{w}\t2\t1\t1\t100\t5a4a\t8\t1\t1\n"
        );
        let mut nodes = parse_node_moves(&table, 0, Some(&BTreeSet::new())).unwrap();
        assert_eq!(nodes[B].moves[0].value_kind, "tree:2");
        assert_eq!(nodes[B].moves[1].value_kind, "single");
        assert_eq!(nodes[w].moves[0].value_kind, "unknown");
        assert!(
            parse_node_moves(&table.replace("\t1\t1\n", "\t2\t1\n"), 0, Some(&BTreeSet::new()))
                .is_err()
        );
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
        assert_eq!(find(&rows, B, "5i4i").unwrap().get("value_kind"), "tree:3");
        let (_, _, summary) = register(rows, &nodes, &options()).unwrap();
        assert_eq!(summary.evals_updated, 0);
    }

    #[test]
    fn propagation_uses_child_remaining_and_never_flips_keys() {
        const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let destination = child(ROOT, "5i4i").unwrap();
        let flipped = rshogi_book::flipped_key(&format!("{destination} 1")).unwrap();
        let flipped = crate::book_db::strip_ply(&flipped);
        let header = "key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n";
        for expanded in [0, 1] {
            let parent = format!("{ROOT}\t1\t4\t0\t100\t5i4i\t10\t{expanded}\t1\n");
            let text = format!("{header}{parent}{destination}\t2\t1\t3\t100\t5a4a\t-10\t0\t1\n");
            let nodes = parse_node_moves(&text, 0, Some(&BTreeSet::new())).unwrap();
            assert_eq!(nodes[ROOT].moves[0].value_kind, "tree:4");
            let (rows, _, _) = register(vec![], &nodes, &options()).unwrap();
            assert_eq!(find(&rows, ROOT, "5i4i").unwrap().get("value_kind"), "tree:4");

            let text = format!("{header}{parent}{flipped}\t2\t1\t3\t100\t5i4i\t-10\t0\t1\n");
            let nodes = parse_node_moves(&text, 0, Some(&BTreeSet::new())).unwrap();
            assert_eq!(
                nodes[ROOT].moves[0].value_kind,
                if expanded == 0 { "single" } else { "unknown" }
            );
        }
    }

    #[test]
    fn destinations_omitted_from_node_moves_have_unknown_depth() {
        const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let destination = child(ROOT, "5i4i").unwrap();
        for expanded in [0, 1] {
            let text = format!(
                "key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n{ROOT}\t1\t0\t2\t100\t5i4i\t-30000\t{expanded}\t1\n"
            );
            let nodes =
                parse_node_moves(&text, 0, Some(&BTreeSet::from([destination.clone()]))).unwrap();
            assert_eq!(nodes[ROOT].moves[0].value_kind, "unknown");
            let nodes = parse_node_moves(&text, 0, None).unwrap();
            assert_eq!(nodes[ROOT].moves[0].value_kind, "unknown");
        }
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
