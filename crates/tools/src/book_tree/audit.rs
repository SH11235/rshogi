//! 定跡の元データの読みの値を `book_backprop` の後退解析で伝播し、main が伝播後の 1 位から
//! どれだけ負けるかを点検し、指定時には main を置き換える。

use super::master_table::{
    self, BookMove, MasterPosition, Row, format_master, parse_master, positions_from_rows,
};
use super::refutation::{self, RefutationOutputs};
use super::rejoin::WeightArgs;
use super::reply_graph::apply_usi;
use super::schema::Schema;
use super::targets::{DEFAULT_PRIORITY, MAX_DEPTH, MAX_PRIORITY};
use crate::book_backprop::{
    DrawValues, Graph, MergeMode, PropagationStats, delta_buckets, graph_from_entries, parse_row,
    percentile, propagate_values, retain_known, write_book_atomic,
};
use crate::book_db::{position_from_sfen, static_position_from_sfen, strip_ply};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use rayon::prelude::*;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub(super) struct AuditArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 伝播後の 1 位を main にして元データを保存する
    #[arg(long, requires = "since")]
    apply: bool,
    /// curated の行がある局面でも main の置換・alt の降格を行う
    #[arg(long)]
    replace_curated: bool,
    /// 悪い alt を record にする (--apply なしでは件数だけ確認)
    #[arg(long)]
    demote_alts: bool,
    /// alt を降格する値の上限 (手番側からの値)
    #[arg(long, default_value_t = -150, allow_hyphen_values = true)]
    bad_eval: i32,
    /// 置換の note に付けるタグ (--apply 時は必須)
    #[arg(long)]
    since: Option<String>,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
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
    /// 反駁の表 (TSV) の出力先。伝播後の 1 位が伸ばしていない手で、伸ばした手の最善を
    /// --refutation-margin 以上上回る局面を書く
    #[arg(long)]
    refutation_table: Option<PathBuf>,
    /// 反駁の線。伸ばしていない手の最善が伸ばした手の最善をこの値以上上回る局面を出す
    #[arg(long, default_value_t = 1)]
    refutation_margin: i32,
    /// 反駁の局面ごとに、伸ばしていない手を指した後の局面を根にした `tree enqueue --from` の行 (JSON Lines) の出力先
    #[arg(long)]
    enqueue_out: Option<PathBuf>,
    /// キューの行の depth
    #[arg(
        long,
        default_value_t = 2,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(0..=MAX_DEPTH as u64)
    )]
    refutation_depth: usize,
    /// キューの行の数の上限 (表の順で先から)
    #[arg(long)]
    refutation_max: Option<usize>,
    /// キューの行の priority の基準。表の順位 (0 始まり、99 で頭打ち) を足す
    #[arg(
        long,
        default_value_t = DEFAULT_PRIORITY,
        value_parser = clap::value_parser!(u32).range(0..=i64::from(MAX_PRIORITY - 99))
    )]
    priority: u32,
    /// 読みの値の無い手を指した後の、元データに無い相手の手番の局面を、応手が元データの局面に着く手だけで補って伝播する
    /// (補った局面の値は定跡に無い応手を見ない分だけ甘い)
    #[arg(long, conflicts_with = "apply")]
    fill_replies: bool,
    #[command(flatten)]
    weight: WeightArgs,
    /// スレッド数 (0 = 全コア)。結果はスレッド数によらない
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    /// 1 回の読みの値 (book 内に子局面が無い)。
    Eval,
    /// book 内の子局面から伝播した値。
    Propagated,
}

impl Origin {
    pub(super) fn as_str(self) -> &'static str {
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
    /// 1 位と同値で選ばれず、`value` を -1 した手か (補正前の値は `value + 1`)。
    pub tie_lowered: bool,
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
    /// 伝播のために補った、元データに無い相手の手番の局面の数。
    pub reply_positions: usize,
}

pub(super) fn run(args: AuditArgs) -> Result<()> {
    super::reply_graph::thread_pool(args.threads)?.install(|| run_in_pool(args))
}

fn run_in_pool(args: AuditArgs) -> Result<()> {
    ensure!(!args.apply || args.since.is_some(), "--apply には --since が必要です");
    if let Some(since) = &args.since {
        ensure!(!since.trim().is_empty(), "--since を指定してください");
        ensure!(!since.contains(['\t', '\r', '\n']), "since にタブや改行は使えません");
    }
    ensure!(!(args.apply && args.fill_replies), "--apply と --fill-replies は併用できません");
    ensure!(
        !(args.demote_alts && args.fill_replies),
        "--demote-alts と --fill-replies は併用できません"
    );
    let schema = Schema::load(args.schema.as_deref())?;
    ensure!(args.line > 0, "--line は 1 以上にしてください");
    ensure!(args.refutation_margin > 0, "--refutation-margin は 1 以上にしてください");
    let refutation = args.refutation_table.is_some() || args.enqueue_out.is_some();
    ensure!(
        refutation || args.weight.opp_freq.is_none(),
        "--opp-freq は --refutation-table か --enqueue-out と一緒に使います"
    );
    args.weight.check()?;
    let mut inputs = vec![("--master", args.master.as_path())];
    if let Some(freq) = &args.weight.opp_freq {
        inputs.push(("--opp-freq", freq.as_path()));
    }
    let schema_paths = args.schema.as_deref().map(super::schema::paths);
    inputs.extend(schema_paths.iter().flatten().map(|p| ("--schema", p.as_path())));
    let outputs: Vec<(&str, &Path)> = [
        ("--table", Some(args.table.as_path())),
        ("--out-db", args.out_db.as_deref()),
        ("--refutation-table", args.refutation_table.as_deref()),
        ("--enqueue-out", args.enqueue_out.as_deref()),
    ]
    .into_iter()
    .filter_map(|(name, path)| Some((name, path?)))
    .collect();
    for (i, (name, path)) in outputs.iter().enumerate() {
        let mut others = inputs.clone();
        others.extend(outputs[..i].iter().copied());
        reject_same_file(path, &others).with_context(|| format!("{name} の出力先"))?;
    }
    let full_rows = args.apply || args.demote_alts;
    let (original, mut rows, positions, curated_records) = if full_rows {
        let original = std::fs::read(&args.master)?;
        let rows = parse_master(std::str::from_utf8(&original)?)?;
        master_table::validate(&rows)?;
        for row in &rows {
            schema.validate_row(row)?;
        }
        let positions =
            positions_from_rows(rows.iter().enumerate().map(|(i, row)| Ok((i + 2, row.clone()))))?;
        (original, rows, positions, BTreeSet::new())
    } else {
        let reader = std::io::BufReader::new(std::fs::File::open(&args.master)?);
        let mut rows = Vec::new();
        let mut curated_records = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let positions = positions_from_rows(master_table::read_rows(reader)?.map(|entry| {
            let (at, row) = entry?;
            schema.validate_row(&row)?;
            ensure!(
                seen.insert((row.get("key").to_owned(), row.get("move").to_owned())),
                "同じ手の行が重なっています: {} {}",
                row.get("key"),
                row.get("move")
            );
            if row.get("role") != "record" {
                rows.push(row.clone());
            } else if row.get("tier") == "curated" {
                // record の列を保持せず、置換を保護する局面だけを覚える。
                curated_records.insert(row.get("key").to_owned());
            }
            Ok((at, row))
        }))?;
        drop(seen);
        curated_records.retain(|key| positions.0.contains_key(key));
        master_table::validate(&rows)?;
        (Vec::new(), rows, positions, curated_records)
    };
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let (findings, summary, book) = audit(&positions, draws, args.line, args.fill_replies)?;
    let (changes, replacements) = plan_replacements(
        &rows,
        &curated_records,
        &findings,
        &schema,
        args.since.as_deref().unwrap_or("確認"),
        args.replace_curated,
    )?;
    write_atomic(&args.table, &format_table(&findings))?;
    if let Some(out) = &args.out_db {
        write_audited_db(out, &positions.0, &book)?;
    }
    print!("{}", format_summary(&summary, args.line, draws));
    if refutation {
        let outputs = RefutationOutputs {
            table: args.refutation_table.as_deref(),
            enqueue_out: args.enqueue_out.as_deref(),
            margin: args.refutation_margin,
            depth: args.refutation_depth,
            max: args.refutation_max,
            priority: args.priority,
            weight: &args.weight,
        };
        print!("{}", refutation::write_outputs(&rows, &positions.0, &book, &outputs)?);
    }
    rewrite_rows(&mut rows, &changes);
    let demotions = if args.demote_alts {
        Some(demote_alts(&mut rows, &positions.0, &book, &schema, &args)?)
    } else {
        None
    };
    master_table::validate(&rows)?;
    if args.apply && (!changes.is_empty() || demotions.as_ref().is_some_and(|d| d.total() > 0)) {
        write_master(&args.master, &original, &rows)?;
    }
    if let Some(demotions) = demotions {
        println!("{}", demotions.format());
    }
    println!(
        "{}: {}",
        if args.apply {
            "元データに反映"
        } else {
            "確認だけ (--apply なし)"
        },
        replacements.format()
    );
    Ok(())
}

#[derive(Debug, Default, PartialEq)]
struct ReplacementSummary {
    replaced: BySide,
    curated: usize,
    skipped_curated: usize,
    protected: usize,
    eval: usize,
    missing: usize,
}

impl ReplacementSummary {
    fn format(&self) -> String {
        format!(
            "置換する局面: 先手番 {}・後手番 {} (うち curated {}), curated なので替えなかった局面 {}, source が置換対象でない局面 {}, 1 位が伸ばしていない手なので替えなかった局面 {}, 対象行が無いか main・alt でない局面 {}",
            self.replaced[0],
            self.replaced[1],
            self.curated,
            self.skipped_curated,
            self.protected,
            self.eval,
            self.missing
        )
    }
}

type Changes = BTreeMap<(String, String), Row>;

fn plan_replacements(
    rows: &[Row],
    curated_records: &BTreeSet<String>,
    findings: &[Finding],
    schema: &Schema,
    since: &str,
    replace_curated: bool,
) -> Result<(Changes, ReplacementSummary)> {
    let selected: BTreeMap<_, _> =
        rows.iter().map(|row| ((row.get("key"), row.get("move")), row)).collect();
    let curated_keys: BTreeSet<_> = rows
        .iter()
        .filter(|row| row.get("tier") == "curated")
        .map(|row| row.get("key"))
        .collect();
    let mut changes = BTreeMap::new();
    let mut summary = ReplacementSummary::default();
    // 役割の交換では局面の値は変わらない。assign_outputs の同値 -1 補正だけは
    // 手の間を移るので、再点検で差 1 が出ることがある。
    for f in findings {
        if f.best.origin == Origin::Eval {
            summary.eval += 1;
            continue;
        }
        let main = selected.get(&(f.key.as_str(), f.main.mv.as_str()));
        let best = selected.get(&(f.key.as_str(), f.best.mv.as_str()));
        let (Some(&main), Some(&best)) = (main, best) else {
            summary.missing += 1;
            continue;
        };
        if main.get("role") != "main" || best.get("role") != "alt" {
            summary.missing += 1;
            continue;
        }
        if !schema.replaceable(main.get("source")) {
            summary.protected += 1;
            continue;
        }
        if !replace_curated
            && (curated_keys.contains(f.key.as_str()) || curated_records.contains(&f.key))
        {
            summary.skipped_curated += 1;
            continue;
        }
        let tier = main.get("tier");
        let curated = tier == "curated";
        let mut old = main.clone();
        let mut new = best.clone();
        old.set("role", "alt".into());
        old.set("tier", "wide".into());
        new.set("role", "main".into());
        new.set("tier", tier.into());
        for (row, reason) in [
            (
                &mut old,
                format!(
                    "伝播後の 1 位でないので main から alt{}",
                    if curated {
                        " (curated から wide)"
                    } else {
                        ""
                    }
                ),
            ),
            (&mut new, "伝播後の 1 位なので alt から main".into()),
        ] {
            let note = row.get("note");
            row.set(
                "note",
                format!("{note}{}{since}: {reason}", if note.is_empty() { "" } else { "。" }),
            );
            schema.validate_row(row)?;
        }
        changes.insert((f.key.clone(), f.main.mv.clone()), old);
        changes.insert((f.key.clone(), f.best.mv.clone()), new);
        summary.replaced[usize::from(f.key.split_whitespace().nth(1) == Some("w"))] += 1;
        summary.curated += usize::from(curated);
    }
    Ok((changes, summary))
}

fn rewrite_rows(rows: &mut [Row], changes: &Changes) {
    for row in rows {
        if let Some(updated) = changes.get(&(row.get("key").into(), row.get("move").into())) {
            *row = updated.clone();
        }
    }
}

fn write_master(path: &Path, original: &[u8], rows: &[Row]) -> Result<()> {
    master_table::validate(rows)?;
    let text = format_master(rows);
    ensure!(std::fs::read(path)? == original, "元データが点検中に変更されたため保存しません");
    write_atomic(path, &text)
}

#[derive(Debug, Default, PartialEq)]
struct DemotionSummary {
    bad: BySide,
    gap: BySide,
    unknown: usize,
    protected: usize,
    skipped_curated: usize,
}

impl DemotionSummary {
    fn total(&self) -> usize {
        self.bad.iter().chain(&self.gap).sum()
    }

    fn format(&self) -> String {
        format!(
            "record に降格する alt: 先手番 {}・後手番 {} / 理由 (a) 値: 先手番 {}・後手番 {} / 理由 (b) 差: 先手番 {}・後手番 {} / 値が不明 {} / source が保護対象 {} / curated なので降格しなかった局面 {}",
            self.bad[0] + self.gap[0],
            self.bad[1] + self.gap[1],
            self.bad[0],
            self.bad[1],
            self.gap[0],
            self.gap[1],
            self.unknown,
            self.protected,
            self.skipped_curated
        )
    }
}

fn demote_alts(
    rows: &mut [Row],
    positions: &BTreeMap<String, MasterPosition>,
    book: &AuditedBook,
    schema: &Schema,
    args: &AuditArgs,
) -> Result<DemotionSummary> {
    let mut indices: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        indices.entry(row.get("key")).or_default().push(i);
    }
    let groups: Vec<_> = positions
        .iter()
        .map(|(key, position)| {
            (key, position, indices.get(key.as_str()).cloned().unwrap_or_default())
        })
        .collect();
    drop(indices);
    let mut summary = DemotionSummary::default();
    for ((key, position, indices), known) in groups.into_iter().zip(&book.known) {
        if !args.replace_curated && indices.iter().any(|&i| rows[i].get("tier") == "curated") {
            summary.skipped_curated += 1;
            continue;
        }
        let moves = audited_moves(&book.graph, key, position, known)?;
        let values: BTreeMap<_, _> = moves
            .iter()
            .flatten()
            .map(|m| (m.mv.as_str(), i64::from(m.value) + i64::from(m.tie_lowered)))
            .collect();
        let best = moves
            .iter()
            .flatten()
            .filter(|m| m.origin == Origin::Propagated || m.mv == position.moves[0].mv)
            .map(|m| values[m.mv.as_str()])
            .max();
        let side = usize::from(key.split_whitespace().nth(1) == Some("w"));
        for i in indices {
            let row = &mut rows[i];
            // 置換前の main は、役割が alt に変わっても候補として残す。
            if row.get("role") != "alt" || row.get("move") == position.moves[0].mv {
                continue;
            }
            let Some(&value) = values.get(row.get("move")) else {
                summary.unknown += 1;
                continue;
            };
            let Some(best) = best else {
                continue;
            };
            let gap = best - value;
            let by_gap = gap >= i64::from(args.line);
            let by_bad = value <= i64::from(args.bad_eval) && value < best;
            if !by_gap && !by_bad {
                continue;
            }
            if !schema.replaceable(row.get("source")) {
                summary.protected += 1;
                continue;
            }
            row.set("role", "record".into());
            let note = row.get("note");
            let since = args.since.as_deref().unwrap_or("確認");
            row.set("note", format!("{note}{}{since}: 伝播後の値が悪いので alt から record (基準との差 {gap} / 値 {value})",
                if note.is_empty() { "" } else { "。" }));
            schema.validate_row(row)?;
            if by_gap {
                summary.gap[side] += 1;
            } else {
                summary.bad[side] += 1;
            }
        }
    }
    Ok(summary)
}

/// 出力先が入力のどれかと同じファイルなら止める (出力の書き出しで入力を壊さないため)。
pub(super) fn reject_same_file(output: &Path, inputs: &[(&str, &Path)]) -> Result<()> {
    let resolved = resolve_output(output)?;
    for (name, input) in inputs {
        ensure!(resolved != resolve(input), "出力先 {} が {name} と同じです", output.display());
    }
    Ok(())
}

/// 出力先の親ディレクトリを作ってから [`resolve`] する。親が無いと同じファイルを指す別の書き方を見分けられない。
fn resolve_output(path: &Path) -> Result<PathBuf> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("{} を作れません", parent.display()))?;
    }
    Ok(resolve(path))
}

/// 同じファイルかを比べるためのパス。まだ無いファイルは親ディレクトリを解決して名前を付ける。
fn resolve(path: &Path) -> PathBuf {
    if let Ok(path) = path.canonicalize() {
        return path;
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    match (parent.canonicalize(), path.file_name()) {
        (Ok(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// 点検後のグラフと、元データの手との対応。
pub(super) struct AuditedBook {
    pub graph: Graph,
    pub known: Vec<Vec<bool>>,
}

fn write_audited_db(
    path: &Path,
    positions: &BTreeMap<String, MasterPosition>,
    book: &AuditedBook,
) -> Result<()> {
    write_book_atomic(path, |mut writer| {
        writeln!(writer, "{}", crate::book_backprop::BOOK_HEADER)?;
        for ((key, position), known) in positions.iter().zip(&book.known) {
            let Some(node) = book.graph.find(key) else {
                continue;
            };
            writeln!(writer, "sfen {}", book.graph.sfen(node))?;
            let mut values = book.graph.moves(node);
            let mut rows = Vec::new();
            for (i, m) in position.moves.iter().enumerate().filter(|(i, _)| known[*i]) {
                let value = values.next().context("内部エラー: 手の数が合いません")?.new;
                let source = format!(
                    "{} none {} 1 {}",
                    m.mv,
                    m.eval.unwrap_or(0),
                    if i == 0 { 2 } else { 1 }
                );
                rows.push((source, value));
            }
            rows.sort_by(|a, b| {
                let (a, b) = (parse_row(&a.0), parse_row(&b.0));
                b.count
                    .cmp(&a.count)
                    .then_with(|| a.move_usi.unwrap_or("none").cmp(b.move_usi.unwrap_or("none")))
            });
            for (source, value) in rows {
                let fields = parse_row(&source);
                writeln!(
                    writer,
                    "{} {} {value} {} {}",
                    fields.move_usi.unwrap_or("none"),
                    fields.ponder_usi.unwrap_or("none"),
                    fields.depth,
                    fields.count
                )?;
            }
        }
        writer.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    })
}

/// 値の分かる手を求める。読みの値のある合法手と、book 内の子局面に値の分かる手がある手。
fn known_moves(positions: &[(&str, &MasterPosition)], graph: &Graph) -> Result<Vec<Vec<bool>>> {
    let node_of: Vec<usize> = positions
        .iter()
        .map(|&(key, _)| {
            graph.find(key).with_context(|| format!("内部エラー: 局面がありません: {key}"))
        })
        .collect::<Result<_>>()?;
    let mut known: Vec<Vec<bool>> = Vec::with_capacity(positions.len());
    let mut node_known = vec![false; graph.len()];
    // (子局面, 親の局面の番号, 手の番号)。子局面の値が分かったら親の手も分かる。
    let mut waiting: Vec<(usize, usize, usize)> = Vec::new();
    for (index, &(_, position)) in positions.iter().enumerate() {
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
                // 直接の子局面の値が分からなくても、反転した局面の値が分かれば、値の分かる手だけを
                // 書く 2 回目のグラフでは辺が反転した局面に張られる。
                if !edge.via_flip
                    && let Some(flipped) = flipped_node(graph, edge.to)
                {
                    waiting.push((flipped, index, i));
                }
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

/// 局面の反転した局面。book に無ければ `None`。
fn flipped_node(graph: &Graph, node: usize) -> Option<usize> {
    let flipped = rshogi_book::flipped_key(graph.sfen(node))?;
    graph.find(strip_ply(&flipped)).filter(|&v| v != node)
}

/// [`propagate`] の結果。
struct Propagated {
    graph: Graph,
    /// 元データの局面ごと (key 順)・手ごとの値の分かる手。補った局面は含まない。
    known: Vec<Vec<bool>>,
    illegal_moves: usize,
    stats: PropagationStats,
    /// 補った相手の手番の局面の数。
    reply_positions: usize,
}

/// 元データの手を指した後の局面のうち、元データに無いもの。
enum Leaf<'a> {
    /// 補わない局面 (読みの値のある手の行き先と、行き先を反転した局面)。
    Blocked(&'a str),
    /// 補う候補 (読みの値の無い手の行き先)。
    Candidate(&'a str),
}

/// 局面の手の行き先のうち元データに無いものを `visit` に渡す。
fn visit_leaves(
    positions: &BTreeMap<String, MasterPosition>,
    key: &str,
    position: &MasterPosition,
    mut visit: impl FnMut(Leaf<'_>),
) -> Result<()> {
    let mut pos = position_from_sfen(&format!("{key} 1"))?;
    for m in &position.moves {
        // 非合法手は book_backprop が数えて除外する。
        let Ok(mv) = apply_usi(&mut pos, &m.mv) else {
            continue;
        };
        let child = pos.to_sfen();
        let child_key = strip_ply(&child);
        let flipped = rshogi_book::flipped_key(&child);
        let flipped = flipped.as_deref().map(strip_ply).filter(|&f| f != child_key);
        if !positions.contains_key(child_key) && !flipped.is_some_and(|f| positions.contains_key(f))
        {
            // book_backprop は直接の子局面が無いと反転した局面に辺を張るので、反転した局面も補わない。
            if let Some(flipped) = flipped {
                visit(Leaf::Blocked(flipped));
            }
            visit(if m.eval.is_some() {
                Leaf::Blocked(child_key)
            } else {
                Leaf::Candidate(child_key)
            });
        }
        pos.undo_move(mv);
    }
    Ok(())
}

/// 読みの値の無い手を指した後の、元データに無い局面 (反転した局面も無いもの) を、相手の合法手のうち元データの局面に
/// 着く手だけを持つ局面として補う。元データは自分の手番の局面にしか行を持たないことが多く、補わないと先の読みの値が
/// 相手の手番の局面で途切れる。補った値は定跡に無い応手を見ない分だけ甘いので、読みの値のある手が着く局面は補わない。
/// 反転した局面が元データにある局面へは応手を張らない (千日手の値が先後で違うと、反転で合流した循環を解けない)。
fn reply_positions(
    positions: &BTreeMap<String, MasterPosition>,
) -> Result<BTreeMap<String, MasterPosition>> {
    // 一度に持つ行き先を塊の局面の分に抑える。
    const CHUNK: usize = 1 << 14;
    let flipped_in_master = |sfen: &str| {
        rshogi_book::flipped_key(sfen).is_some_and(|flipped| {
            let flipped = strip_ply(&flipped);
            flipped != strip_ply(sfen) && positions.contains_key(flipped)
        })
    };
    // 補わない局面は key の hash で持つ。衝突は補わない側に倒れるだけ。
    let hash = |key: &str| {
        let mut hasher = std::hash::DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    };
    let entries: Vec<(&String, &MasterPosition)> = positions.iter().collect();
    let mut blocked: Vec<u64> = Vec::new();
    for chunk in entries.chunks(CHUNK) {
        let part: Vec<Vec<u64>> = chunk
            .par_iter()
            .map(|&(key, position)| -> Result<Vec<u64>> {
                let mut out = Vec::new();
                visit_leaves(positions, key, position, |leaf| {
                    if let Leaf::Blocked(k) = leaf {
                        out.push(hash(k));
                    }
                })?;
                Ok(out)
            })
            .collect::<Result<_>>()?;
        blocked.extend(part.into_iter().flatten());
    }
    blocked.sort_unstable();
    blocked.dedup();
    // 別の局面から同じ局面に来たら、手数の小さい方を残す (並列の順によらず決める)。
    let mut candidates: BTreeMap<String, u32> = BTreeMap::new();
    for chunk in entries.chunks(CHUNK) {
        let part: Vec<Vec<(String, u32)>> = chunk
            .par_iter()
            .map(|&(key, position)| -> Result<Vec<(String, u32)>> {
                let ply = position.ply.parse::<u32>().map_or(1, |n| n.saturating_add(1));
                let mut out = Vec::new();
                visit_leaves(positions, key, position, |leaf| {
                    if let Leaf::Candidate(k) = leaf
                        && blocked.binary_search(&hash(k)).is_err()
                    {
                        out.push((k.to_owned(), ply));
                    }
                })?;
                Ok(out)
            })
            .collect::<Result<_>>()?;
        for (key, ply) in part.into_iter().flatten() {
            let entry = candidates.entry(key).or_insert(ply);
            *entry = (*entry).min(ply);
        }
    }
    let replies: Vec<Option<(String, MasterPosition)>> = candidates
        .into_par_iter()
        .map(|(key, ply)| -> Result<Option<(String, MasterPosition)>> {
            let mut pos = position_from_sfen(&format!("{key} 1"))?;
            let mut legal = MoveList::new();
            generate_legal_all(&pos, &mut legal);
            let mut moves = Vec::new();
            for &reply in legal.iter() {
                pos.do_move(reply, pos.gives_check(reply));
                let to = pos.to_sfen();
                if positions.contains_key(strip_ply(&to)) && !flipped_in_master(&to) {
                    moves.push(BookMove {
                        mv: reply.to_usi(),
                        eval: None,
                    });
                }
                pos.undo_move(reply);
            }
            Ok((!moves.is_empty()).then(|| {
                let position = MasterPosition {
                    ply: ply.to_string(),
                    source: String::new(),
                    moves,
                };
                (key, position)
            }))
        })
        .collect::<Result<_>>()?;
    Ok(replies.into_iter().flatten().collect())
}

/// 局面の値の分かる手だけを伝播する。(伝播後のグラフ, 局面ごと・手ごとの値の分かる手, 非合法手の数, 伝播の集計)。
fn propagate_entries(
    all: &[(&str, &MasterPosition)],
    draws: DrawValues,
) -> Result<(Graph, Vec<Vec<bool>>, usize, PropagationStats)> {
    let mut graph =
        graph_from_entries(all.iter().map(|&(key, p)| {
            (key, p.ply.as_str(), p.moves.iter().map(|m| (m.mv.as_str(), m.eval)))
        }))?;
    let illegal_moves = graph.illegal_moves;
    let known = known_moves(all, &graph)?;
    retain_known(&mut graph, &known)?;
    let stats = propagate_values(&mut graph, draws, MergeMode::Replace)?;
    Ok((graph, known, illegal_moves, stats))
}

/// 元データの値の分かる手だけを伝播する。`fill_replies` なら元データに無い相手の手番の局面を補う
/// ([`reply_positions`])。key が正規の SFEN でなければ止まる。
fn propagate(
    positions: &BTreeMap<String, MasterPosition>,
    draws: DrawValues,
    fill_replies: bool,
) -> Result<Propagated> {
    let keys: Vec<&String> = positions.keys().collect();
    // 並べて調べ、エラーは key の順で最初のものを返す (並べた順で選ぶと実行ごとに変わり得る)。
    let checks: Vec<Result<()>> = keys
        .par_iter()
        .map(|key| -> Result<()> {
            let position = static_position_from_sfen(&format!("{key} 1"))
                .with_context(|| format!("key を局面として読めません: {key}"))?;
            // 正規の表記でない key の局面には book_backprop が子局面の辺を張らず、値が黙って途切れる。
            ensure!(
                strip_ply(&position.to_sfen()) == key.as_str(),
                "key が正規の SFEN (持ち駒の並びなど) ではありません: {key}"
            );
            Ok(())
        })
        .collect();
    checks.into_iter().collect::<Result<()>>()?;
    let replies = if fill_replies {
        reply_positions(positions)?
    } else {
        BTreeMap::new()
    };
    let mut all: Vec<(&str, &MasterPosition)> =
        positions.iter().chain(&replies).map(|(key, p)| (key.as_str(), p)).collect();
    // 補った局面は元データと key が重ならないので、key 順に並べれば元データの局面の順は保たれる。
    all.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let (graph, known_all, illegal_moves, stats) = propagate_entries(&all, draws)?;
    let known = all
        .iter()
        .zip(known_all)
        .filter(|((key, _), _)| positions.contains_key(*key))
        .map(|(_, known)| known)
        .collect();
    let reply_count = replies.len();
    Ok(Propagated {
        graph,
        known,
        illegal_moves,
        stats,
        reply_positions: reply_count,
    })
}

/// 元データの局面の伝播後の値 (手番の側から)。値の分からない手は入れない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MasterValue {
    /// main の値。
    pub main: Option<i32>,
    /// 値の分かる手のうち最大の値。
    pub best: Option<i32>,
}

/// 元データを `audit` と同じ規則で伝播し、局面ごとの main と 1 位の値を返す。
pub(super) fn master_values(
    (positions, _): &(BTreeMap<String, MasterPosition>, usize),
    draws: DrawValues,
) -> Result<BTreeMap<String, MasterValue>> {
    let Propagated { graph, known, .. } = propagate(positions, draws, false)?;
    let mut values = BTreeMap::new();
    for ((key, _), known) in positions.iter().zip(&known) {
        let Some(node) = graph.find(key) else {
            values.insert(
                key.clone(),
                MasterValue {
                    main: None,
                    best: None,
                },
            );
            continue;
        };
        // グラフには値の分かる手だけが元データの順 (main が先頭) で入っている。
        let moves: Vec<i32> = graph.moves(node).map(|m| m.new).collect();
        values.insert(
            key.clone(),
            MasterValue {
                main: known[0].then(|| moves[0]),
                best: moves.iter().copied().max(),
            },
        );
    }
    Ok(values)
}

/// 伝播後のグラフから、局面の手ごとの伝播後の値を元データの手の順 (main が先頭) で取り出す。値の分からない
/// 手は `None`。
pub(super) fn audited_moves(
    graph: &Graph,
    key: &str,
    position: &MasterPosition,
    known: &[bool],
) -> Result<Vec<Option<AuditedMove>>> {
    let Some(node) = graph.find(key) else {
        return Ok(vec![None; position.moves.len()]);
    };
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
                tie_lowered: value.tie_lowered,
            }))
        })
        .collect()
}

/// 元データを `audit` と同じ規則で伝播し、手ごとの伝播後の値 (手番の側から) を返す。`[i][j]` は元データの
/// i 番目 (key 順) の局面の j 番目の手 (main が先頭) で、値の分からない手は `None`。
pub(super) fn evaluate(
    positions: &BTreeMap<String, MasterPosition>,
    draws: DrawValues,
) -> Result<Vec<Vec<Option<i32>>>> {
    let Propagated { graph, known, .. } = propagate(positions, draws, false)?;
    positions
        .iter()
        .zip(&known)
        .map(|((key, position), known)| {
            let Some(node) = graph.find(key) else {
                return Ok(vec![None; position.moves.len()]);
            };
            // グラフには値の分かる手だけが元データの順で入っている。
            let mut values = graph.moves(node);
            known
                .iter()
                .map(|&k| {
                    if !k {
                        return Ok(None);
                    }
                    let value = values.next().context("内部エラー: 手の数が合いません")?;
                    Ok(Some(value.new))
                })
                .collect()
        })
        .collect()
}

/// 元データを伝播して main を点検する。点検の表 (差の大きい順)・要約・伝播後のグラフを返す。
pub(super) fn audit(
    (positions, records): &(BTreeMap<String, MasterPosition>, usize),
    draws: DrawValues,
    line: i32,
    fill_replies: bool,
) -> Result<(Vec<Finding>, AuditSummary, AuditedBook)> {
    let mut summary = AuditSummary {
        records: *records,
        ..AuditSummary::default()
    };
    let Propagated {
        graph,
        known,
        illegal_moves,
        stats,
        reply_positions,
    } = propagate(positions, draws, fill_replies)?;
    summary.illegal_moves = illegal_moves;
    summary.reply_positions = reply_positions;
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
        let audited = audited_moves(&graph, key, position, known)?;
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
    Ok((findings, summary, AuditedBook { graph, known }))
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
        "補った相手の手番の局面 (元データに無く、応手が元データの局面に着く): {}",
        summary.reply_positions
    ));
    out(format!(
        "循環 (自明でない強連結成分): {} (最大 {} 局面)",
        summary.nontrivial_sccs, summary.max_scc_size
    ));
    text
}

#[cfg(test)]
mod tests {
    use super::super::master_table::{COLUMNS, mean_eval, parse_master_rows};
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
            writeln!(
                text,
                "{}",
                crate::book_tree::master_table::test_line(&[
                    ("key", key.to_string()),
                    ("ply", "1".into()),
                    ("move", mv.to_string()),
                    ("role", role.to_string()),
                    ("source", "read".into()),
                    ("evals", evals.to_string()),
                    ("tier", "wide".into()),
                    ("since", "t".into())
                ])
            )
            .unwrap();
        }
        text
    }

    fn run_audit(text: &str, line: i32) -> (Vec<Finding>, AuditSummary) {
        run_audit_with(text, line, DrawValues::both(0)).unwrap()
    }

    fn run_audit_with(
        text: &str,
        line: i32,
        draws: DrawValues,
    ) -> Result<(Vec<Finding>, AuditSummary)> {
        let parsed = parse_master_rows(text.as_bytes())?;
        let (findings, summary, _) = audit(&parsed, draws, line, false)?;
        Ok((findings, summary))
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

    /// A を先後反転した局面 (先手番)。
    const A_FLIPPED: &str = "9/3k5/9/9/9/9/9/9/4K4 b -";

    #[test]
    fn values_reach_through_flipped_positions() {
        // 子局面 A は元データに無く、反転した局面だけがある。
        let text = master(&[
            (ROOT, "5i4h", "main", ""),
            (ROOT, "5i6h", "alt", "10"),
            (A_FLIPPED, "5i5h", "main", "30"),
        ]);
        let (findings, summary) = run_audit(&text, 1);
        assert_eq!(summary.known_moves, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].main.value, -30);
        assert_eq!(findings[0].main.origin, Origin::Propagated);

        // A があっても値の分かる手が無ければ、反転した局面の値で分かる手になる。
        let text = master(&[
            (ROOT, "5i4h", "main", ""),
            (ROOT, "5i6h", "alt", "10"),
            (A, "5a5b", "main", ""),
            (A_FLIPPED, "5i5h", "main", "30"),
        ]);
        let (findings, summary) = run_audit(&text, 1);
        assert_eq!(summary.unknown_main, [0, 1]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].main.value, -30);
    }

    #[test]
    fn illegal_main_is_unknown() {
        let text = master(&[(ROOT, "1a1b", "main", "50"), (ROOT, "5i6h", "alt", "10")]);
        let (findings, summary) = run_audit(&text, 1);
        assert!(findings.is_empty());
        assert_eq!(summary.illegal_moves, 1);
        assert_eq!(summary.unknown_main, [1, 0]);
    }

    #[test]
    fn cycles_use_the_draw_value_of_each_side() {
        // ROOT → 5i4h → 5a4a → 4h5i → 4a5a で ROOT に戻る。先手は千日手 (-70) を出口 (-100) より好む。
        let c = "5k3/9/9/9/9/9/9/5K3/9 b -";
        let d = "5k3/9/9/9/9/9/9/9/4K4 w -";
        let text = master(&[
            (ROOT, "5i6h", "main", "-100"),
            (ROOT, "5i4h", "alt", ""),
            (A, "5a4a", "main", ""),
            (c, "4h5i", "main", ""),
            (d, "4a5a", "main", ""),
        ]);
        let draws = DrawValues {
            black: -70,
            white: 0,
        };
        let (findings, summary) = run_audit_with(&text, 1, draws).unwrap();
        assert_eq!(summary.nontrivial_sccs, 1);
        assert_eq!(summary.unknown_main, [0, 0]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].key, ROOT);
        assert_eq!((findings[0].best.mv.as_str(), findings[0].best.value), ("5i4h", -70));
        assert_eq!(findings[0].gap(), 30);
    }

    #[test]
    fn in_memory_filter_matches_rebuilt_graph_for_known_move_combinations() {
        use crate::book_backprop::{BackpropOptions, read_graph, write_backprop_book};
        let dir = tempfile::tempdir().unwrap();
        let draws = DrawValues {
            black: -70,
            white: 0,
        };
        let c = "5k3/9/9/9/9/9/9/5K3/9 b -";
        let d = "5k3/9/9/9/9/9/9/9/4K4 w -";
        for mask in 0..256u32 {
            let candidates = [
                (ROOT, "5i4h", "main"),
                (ROOT, "5i6h", "alt"),
                (A, "5a5b", "main"),
                (A, "5a4a", "alt"),
                (B, "4h5i", "main"),
                (A_FLIPPED, "5i5h", "main"),
                (c, "4h5i", "main"),
                (d, "4a5a", "main"),
            ];
            let values: Vec<String> = (0..8)
                .map(|i| {
                    if mask & (1 << i) == 0 {
                        String::new()
                    } else {
                        (i * 10 - 30).to_string()
                    }
                })
                .collect();
            let mut rows: Vec<_> = candidates
                .iter()
                .zip(&values)
                .map(|(&(k, m, r), e)| (k, m, r, e.as_str()))
                .collect();
            rows.push((ROOT, "bad", "alt", "99"));
            rows.push((ROOT, "5i5h 5a5b", "alt", "99"));
            rows.push((ROOT, " 5i4i ", "alt", "12"));
            let (mut positions, _) = parse_master_rows(master(&rows).as_bytes()).unwrap();
            if mask % 2 == 0 {
                positions.get_mut(ROOT).unwrap().ply = "1 ".into();
            }
            let text_db = |known: Option<&[Vec<bool>]>| {
                let mut text = format!("{}\n", crate::book_backprop::BOOK_HEADER);
                for (v, (key, p)) in positions.iter().enumerate() {
                    let moves: Vec<_> = p
                        .moves
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| known.is_none_or(|k| k[v][*i]))
                        .collect();
                    if moves.is_empty() {
                        continue;
                    }
                    writeln!(text, "sfen {key} {}", p.ply).unwrap();
                    for (i, m) in moves {
                        writeln!(
                            text,
                            "{} none {} {} {}",
                            m.mv,
                            m.eval.unwrap_or(0),
                            i32::from(m.eval.is_some() || known.is_some()),
                            if i == 0 { 2 } else { 1 }
                        )
                        .unwrap();
                    }
                }
                text
            };
            let all_path = dir.path().join("all.db");
            std::fs::write(&all_path, text_db(None)).unwrap();
            let all = read_graph(&all_path, BackpropOptions::default()).unwrap();
            let entries: Vec<(&str, &MasterPosition)> =
                positions.iter().map(|(key, p)| (key.as_str(), p)).collect();
            let known = known_moves(&entries, &all).unwrap();
            let known_path = dir.path().join("known.db");
            std::fs::write(&known_path, text_db(Some(&known))).unwrap();
            let mut rebuilt = read_graph(
                &known_path,
                BackpropOptions {
                    skip_unusable_moves: true,
                    ..BackpropOptions::default()
                },
            )
            .unwrap();
            propagate_values(&mut rebuilt, draws, MergeMode::Replace).unwrap();
            let (memory, actual_known, illegal, _) = propagate_entries(&entries, draws).unwrap();
            assert_eq!(known, actual_known, "mask {mask}");
            assert_eq!(all.illegal_moves, illegal, "mask {mask}");
            assert_eq!(rebuilt.len(), memory.len(), "mask {mask}");
            assert_eq!(rebuilt.flip_edges, memory.flip_edges, "mask {mask}");
            for v in 0..rebuilt.len() {
                assert_eq!(rebuilt.sfen(v), memory.sfen(v), "mask {mask}");
                let values = |g: &Graph| {
                    g.moves(v)
                        .map(|m| (m.old, m.new, m.usable, m.edge.map(|e| (e.to, e.via_flip))))
                        .collect::<Vec<_>>()
                };
                assert_eq!(values(&rebuilt), values(&memory), "mask {mask}");
            }
            let old_out = dir.path().join("old.db");
            let new_out = dir.path().join("new.db");
            write_backprop_book(&rebuilt, &old_out).unwrap();
            write_audited_db(
                &new_out,
                &positions,
                &AuditedBook {
                    graph: memory,
                    known,
                },
            )
            .unwrap();
            assert_eq!(
                std::fs::read(&old_out).unwrap(),
                std::fs::read(&new_out).unwrap(),
                "mask {mask}"
            );
        }
    }

    /// `--fill-replies` で点検し、(要約, ROOT の main の伝播後の値) を返す。
    fn fill_audit(rows: &[(&str, &str, &str, &str)]) -> (AuditSummary, Option<i32>) {
        let parsed = parse_master_rows(master(rows).as_bytes()).unwrap();
        let (_, summary, book) = audit(&parsed, DrawValues::both(0), 1, true).unwrap();
        let index = parsed.0.keys().position(|k| k == ROOT).unwrap();
        let main = audited_moves(&book.graph, ROOT, &parsed.0[ROOT], &book.known[index]).unwrap()
            [0]
        .as_ref()
        .map(|m| m.value);
        (summary, main)
    }

    #[test]
    fn fill_replies_propagates_through_missing_opponent_position() {
        // A (後手番) は元データに無い。A の応手 5a5b で着く B の読みの値が ROOT の main に届く。
        let rows = [(ROOT, "5i4h", "main", ""), (B, "4h3h", "main", "50")];
        let (summary, main) = fill_audit(&rows);
        assert_eq!((summary.reply_positions, main), (1, Some(50)));
        let (_, summary) = run_audit(&master(&rows), 1);
        assert_eq!((summary.reply_positions, summary.unknown_main), (0, [1, 0]));
    }

    #[test]
    fn fill_replies_keeps_read_values() {
        // 読みの値のある手が A に着くなら、定跡の応手だけで決まる甘い値で置き換えない。
        let (summary, main) =
            fill_audit(&[(ROOT, "5i4h", "main", "10"), (B, "4h3h", "main", "50")]);
        assert_eq!((summary.reply_positions, main), (0, Some(10)));
        // 別の局面の読みの値のある手が A に着くときも補わない。
        let c = "4k4/9/9/9/9/9/9/6K2/9 b -";
        let (summary, main) = fill_audit(&[
            (ROOT, "5i4h", "main", ""),
            (c, "3h4h", "main", "10"),
            (B, "4h3h", "main", "50"),
        ]);
        assert_eq!((summary.reply_positions, main), (0, None));
    }

    #[test]
    fn fill_replies_keeps_smallest_ply_of_parents() {
        // ROOT (1 手目) と c (9 手目) の読みの値の無い手が同じ A に着く。A は 1 つだけ補い、手数は小さい方。
        let c = "4k4/9/9/9/9/9/9/6K2/9 b -";
        let text = master(&[
            (ROOT, "5i4h", "main", ""),
            (c, "3h4h", "main", ""),
            (B, "4h3h", "main", "50"),
        ]);
        let (mut positions, _) = parse_master_rows(text.as_bytes()).unwrap();
        positions.get_mut(c).unwrap().ply = "9".into();
        let replies = reply_positions(&positions).unwrap();
        assert_eq!(replies.keys().collect::<Vec<_>>(), vec![A]);
        assert_eq!(replies[A].ply, "2");
    }

    #[test]
    fn fill_replies_skips_flipped_destinations_of_book_moves() {
        // d の 5b6b の行き先 (A を反転した局面) は元データに無いので、A を補うと book_backprop がその辺を A に
        // 張り替え、反転で合流する。そのため A も、A を反転した局面も補わない。
        let d = "9/4k4/9/9/9/9/9/9/4K4 w -";
        let (summary, main) = fill_audit(&[
            (ROOT, "5i4h", "main", ""),
            (d, "5b6b", "main", ""),
            (B, "4h3h", "main", "50"),
        ]);
        assert_eq!((summary.reply_positions, main), (0, None));
    }

    #[test]
    fn fill_replies_does_not_reach_flipped_positions() {
        // B に値の分かる手が無く、反転した B にあるとき、A の応手を反転した B へ張り替えさせない。
        let flipped = rshogi_book::flipped_key(&format!("{B} 1")).unwrap();
        let flipped_b = strip_ply(&flipped);
        let (summary, main) = fill_audit(&[
            (ROOT, "5i4h", "main", ""),
            (B, "4h3h", "main", ""),
            (flipped_b, "6b6c", "main", "30"),
        ]);
        assert_eq!((summary.reply_positions, main), (0, None));
    }

    #[test]
    fn rejects_non_canonical_keys() {
        // 正規の表記は持ち駒を飛・角・金・銀・桂・香・歩の順に並べる。
        let text = master(&[("4k4/9/9/9/9/9/9/9/4K4 b PG", "5i5h", "main", "0")]);
        let error = run_audit_with(&text, 1, DrawValues::both(0)).unwrap_err();
        assert!(error.to_string().contains("正規の SFEN"), "{error}");
    }

    fn test_args(dir: &Path) -> AuditArgs {
        AuditArgs {
            master: dir.join("moves.tsv"),
            apply: false,
            replace_curated: true,
            demote_alts: false,
            bad_eval: -150,
            since: None,
            schema: None,
            table: dir.join("audit.tsv"),
            line: 1,
            draw_value_black: -70,
            draw_value_white: 0,
            out_db: None,
            refutation_table: None,
            refutation_margin: 1,
            enqueue_out: None,
            refutation_depth: 2,
            refutation_max: None,
            priority: 100,
            fill_replies: false,
            weight: WeightArgs {
                opp_freq: None,
                min_games: 3,
                max_ply: 80,
                weight_floor: 1e-5,
                min_weight: 0.0,
            },
            threads: 1,
        }
    }

    fn replacement_fixture() -> String {
        let text = master(&[
            (ROOT, "5i6h", "main", "10"),
            (ROOT, "5i4h", "alt", "0"),
            (ROOT, "5i5h", "record", "999"),
            (A, "5a5b", "main", "-80"),
        ]);
        let mut rows = super::super::master_table::parse_master(&text).unwrap();
        rows[0].set("tier", "curated".into());
        rows[0].set("note", "元の注記".into());
        format_master(&rows)
    }

    #[test]
    fn lightweight_audit_matches_full_rows_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let mut rows = parse_master(&replacement_fixture()).unwrap();
        rows[0].set("evals", "100".into());
        let original = format_master(&rows);
        let path = dir.path().join("moves.tsv");
        std::fs::write(&path, &original).unwrap();
        for (name, demote_alts) in [("light", false), ("full", true)] {
            let mut args = test_args(dir.path());
            args.demote_alts = demote_alts;
            args.table = dir.path().join(format!("{name}.tsv"));
            args.out_db = Some(dir.path().join(format!("{name}.db")));
            args.refutation_table = Some(dir.path().join(format!("{name}.refutation.tsv")));
            args.enqueue_out = Some(dir.path().join(format!("{name}.jsonl")));
            run(args).unwrap();
        }
        for suffix in ["tsv", "db", "refutation.tsv", "jsonl"] {
            assert_eq!(
                std::fs::read(dir.path().join(format!("light.{suffix}"))).unwrap(),
                std::fs::read(dir.path().join(format!("full.{suffix}"))).unwrap(),
                "{suffix}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("light.refutation.tsv"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("light.jsonl")).unwrap().lines().count(),
            1
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn lightweight_audit_rejects_duplicate_record_moves() {
        let dir = tempfile::tempdir().unwrap();
        for roles in [
            ["record", "record"],
            ["main", "record"],
            ["record", "main"],
            ["alt", "record"],
            ["record", "alt"],
        ] {
            let other_role = if roles.contains(&"main") {
                "alt"
            } else {
                "main"
            };
            let text = master(&[
                (ROOT, "5i6h", other_role, "10"),
                (ROOT, "5i4h", roles[0], "0"),
                (ROOT, "5i4h", roles[1], "0"),
            ]);
            std::fs::write(dir.path().join("moves.tsv"), text).unwrap();
            for demote_alts in [false, true] {
                let mut args = test_args(dir.path());
                args.demote_alts = demote_alts;
                let error = run(args).unwrap_err().to_string();
                assert!(error.contains("同じ手の行が重なっています"), "{roles:?}: {error}");
            }
            assert!(!dir.path().join("audit.tsv").exists());
        }
    }

    #[test]
    fn curated_records_protect_replacements_without_retaining_rows() {
        let text = replacement_fixture();
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let (findings, _, _) = audit(&positions, DrawValues::both(0), 1, false).unwrap();
        let mut rows = parse_master(&text).unwrap();
        rows[0].set("tier", "wide".into());
        rows[2].set("tier", "curated".into());
        let (_, expected) =
            plan_replacements(&rows, &BTreeSet::new(), &findings, &Schema::default(), "tag", false)
                .unwrap();
        rows.retain(|row| row.get("role") != "record");
        let (_, actual) = plan_replacements(
            &rows,
            &BTreeSet::from([ROOT.to_owned()]),
            &findings,
            &Schema::default(),
            "tag",
            false,
        )
        .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.skipped_curated, 1);
    }

    #[test]
    fn propagated_best_inherits_curated_tier_and_preserves_other_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        let text = replacement_fixture();
        std::fs::write(&path, &text).unwrap();
        let (findings, _) = run_audit(&text, 40);
        assert_eq!(findings.len(), 1);
        let (changes, summary) = plan_replacements(
            &parse_master(&text).unwrap(),
            &BTreeSet::new(),
            &findings,
            &Schema::default(),
            "tag",
            true,
        )
        .unwrap();
        assert_eq!(summary.replaced, [1, 0]);
        assert_eq!(summary.curated, 1);
        let mut rewritten = parse_master(&text).unwrap();
        rewrite_rows(&mut rewritten, &changes);
        write_master(&path, text.as_bytes(), &rewritten).unwrap();
        let after = std::fs::read_to_string(path).unwrap();
        let original = super::super::master_table::parse_master(&text).unwrap();
        let rows = super::super::master_table::parse_master(&after).unwrap();
        super::super::master_table::validate(&rows).unwrap();
        assert_eq!((rows[0].get("role"), rows[0].get("tier")), ("alt", "wide"));
        assert_eq!((rows[1].get("role"), rows[1].get("tier")), ("main", "curated"));
        assert_eq!(
            rows[0].get("note"),
            "元の注記。tag: 伝播後の 1 位でないので main から alt (curated から wide)"
        );
        assert_eq!(rows[1].get("note"), "tag: 伝播後の 1 位なので alt から main");
        for (before, after) in original.iter().zip(&rows) {
            for column in COLUMNS.into_iter().filter(|c| !["role", "tier", "note"].contains(c)) {
                assert_eq!(before.get(column), after.get(column), "{column}");
            }
        }
        assert_eq!(original[2..], rows[2..]);
    }

    #[test]
    fn eval_best_and_protected_source_are_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        let text = replacement_fixture();
        std::fs::write(&path, &text).unwrap();
        let (mut findings, _) = run_audit(&text, 40);
        findings[0].best.origin = Origin::Eval;
        let (changes, summary) = plan_replacements(
            &parse_master(&text).unwrap(),
            &BTreeSet::new(),
            &findings,
            &Schema::default(),
            "tag",
            true,
        )
        .unwrap();
        assert!(changes.is_empty());
        assert_eq!(summary.eval, 1);
        findings[0].best.origin = Origin::Propagated;
        super::super::schema::test_schema(dir.path());
        std::fs::write(
            dir.path().join("sources.tsv"),
            "source\tdescription\treplaceable\nread\t保護\t0\n",
        )
        .unwrap();
        let schema = Schema::load(Some(dir.path())).unwrap();
        let (changes, summary) = plan_replacements(
            &parse_master(&text).unwrap(),
            &BTreeSet::new(),
            &findings,
            &schema,
            "tag",
            true,
        )
        .unwrap();
        assert!(changes.is_empty());
        assert_eq!(summary.protected, 1);
        for mv in ["5i5h", "missing"] {
            findings[0].best.mv = mv.into();
            let (changes, summary) = plan_replacements(
                &parse_master(&text).unwrap(),
                &BTreeSet::new(),
                &findings,
                &Schema::default(),
                "tag",
                true,
            )
            .unwrap();
            assert!(changes.is_empty());
            assert_eq!(summary.missing, 1);
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }

    #[test]
    fn curated_rows_require_opt_in_even_when_record() {
        for index in [0, 2] {
            let mut rows = parse_master(&replacement_fixture()).unwrap();
            rows[0].set("tier", "wide".into());
            rows[index].set("tier", "curated".into());
            let (findings, _) = run_audit(&format_master(&rows), 40);
            let (changes, summary) = plan_replacements(
                &rows,
                &BTreeSet::new(),
                &findings,
                &Schema::default(),
                "tag",
                false,
            )
            .unwrap();
            assert!(changes.is_empty());
            assert_eq!(summary.skipped_curated, 1);
            let (changes, summary) = plan_replacements(
                &rows,
                &BTreeSet::new(),
                &findings,
                &Schema::default(),
                "tag",
                true,
            )
            .unwrap();
            assert_eq!(changes.len(), 2);
            assert_eq!(summary.replaced, [1, 0]);
        }
    }

    #[test]
    fn rewrite_checks_concurrent_changes_and_normalizes_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        let original = replacement_fixture().replace('\n', "\r\n");
        let rows = parse_master(&original).unwrap();
        std::fs::write(&path, &original).unwrap();
        write_master(&path, original.as_bytes(), &rows).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format_master(&rows));
        let concurrent = format!("{} ", format_master(&rows));
        std::fs::write(&path, &concurrent).unwrap();
        assert!(write_master(&path, original.as_bytes(), &rows).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), concurrent);
        let mut invalid = rows.clone();
        invalid[1].set("role", "main".into());
        assert!(write_master(&path, concurrent.as_bytes(), &invalid).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), concurrent);
    }

    #[test]
    fn multiple_replacements_include_white_and_flipped_destination() {
        let dir = tempfile::tempdir().unwrap();
        let mut rows = parse_master(&replacement_fixture()).unwrap();
        let black_rows = rows.clone();
        for row in &black_rows {
            let mut flipped = row.clone();
            let key = rshogi_book::flipped_key(&format!("{} 1", row.get("key"))).unwrap();
            flipped.set("key", strip_ply(&key).into());
            let mv: String = row
                .get("move")
                .chars()
                .map(|c| match c {
                    '1'..='9' => char::from(b'0' + 10 - (c as u8 - b'0')),
                    'a'..='i' => char::from(b'a' + b'i' - c as u8),
                    _ => c,
                })
                .collect();
            flipped.set("move", mv);
            rows.push(flipped);
        }
        std::fs::write(dir.path().join("moves.tsv"), format_master(&rows)).unwrap();
        let mut args = test_args(dir.path());
        args.apply = true;
        args.since = Some("tag".into());
        args.line = 40;
        run(args).unwrap();
        let text = std::fs::read_to_string(dir.path().join("moves.tsv")).unwrap();
        assert!(run_audit(&text, 40).0.is_empty());
        let updated = parse_master(&text).unwrap();
        assert_eq!(updated.iter().filter(|r| r.get("note").contains("alt から main")).count(), 2);

        let text = master(&[
            (ROOT, "5i6h", "main", "-100"),
            (ROOT, "5i4h", "alt", ""),
            (A_FLIPPED, "5i5h", "main", "30"),
        ]);
        let (findings, _) = run_audit(&text, 40);
        assert_eq!(findings.len(), 1);
        let mut rows = parse_master(&text).unwrap();
        let (changes, _) =
            plan_replacements(&rows, &BTreeSet::new(), &findings, &Schema::default(), "tag", true)
                .unwrap();
        rewrite_rows(&mut rows, &changes);
        assert_eq!(rows[1].get("role"), "main");
        assert!(run_audit(&format_master(&rows), 40).0.is_empty());
    }

    #[test]
    fn demotions_use_values_before_tie_adjustment_and_keep_other_columns() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (-140, -150, 40, "record", [1, 0], [0, 0]),
            (100, 60, 40, "record", [0, 0], [1, 0]),
            (100, 100, 1, "alt", [0, 0], [0, 0]),
            (-200, -200, 1, "alt", [0, 0], [0, 0]),
            (-200, -210, 40, "record", [1, 0], [0, 0]),
            (-200, -240, 40, "record", [0, 0], [1, 0]),
        ];
        for (main, alt, line, role, bad, gap) in cases {
            let text = master(&[
                (ROOT, "5i6h", "main", &main.to_string()),
                (ROOT, "5i4h", "alt", &alt.to_string()),
                (ROOT, "5i5h", "record", "-999"),
                (ROOT, "5i4i", "alt", ""),
            ]);
            let positions = parse_master_rows(text.as_bytes()).unwrap();
            let (_, _, book) = audit(&positions, DrawValues::both(0), line, false).unwrap();
            let original = parse_master(&text).unwrap();
            let mut rows = original.clone();
            let mut args = test_args(dir.path());
            args.line = line;
            args.since = Some("tag".into());
            let result =
                demote_alts(&mut rows, &positions.0, &book, &Schema::default(), &args).unwrap();
            assert_eq!(result.bad, bad);
            assert_eq!(result.gap, gap);
            assert_eq!(result.unknown, 1);
            assert_eq!(rows[1].get("role"), role);
            assert_eq!(rows[0], original[0]);
            assert_eq!(rows[2..], original[2..]);
            for column in COLUMNS.into_iter().filter(|c| !["role", "note"].contains(c)) {
                assert_eq!(rows[1].get(column), original[1].get(column));
            }
            if role == "record" {
                assert_eq!(
                    rows[1].get("note"),
                    format!(
                        "tag: 伝播後の値が悪いので alt から record (基準との差 {} / 値 {alt})",
                        main - alt
                    )
                );
            }
        }
    }

    #[test]
    fn demotions_keep_old_and_new_main() {
        let dir = tempfile::tempdir().unwrap();
        super::super::schema::test_schema(dir.path());
        std::fs::write(
            dir.path().join("sources.tsv"),
            "source\tdescription\treplaceable\nread\t保護\t0\n",
        )
        .unwrap();
        let schema = Schema::load(Some(dir.path())).unwrap();
        let text = replacement_fixture();
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let (findings, _, book) = audit(&positions, DrawValues::both(0), 40, false).unwrap();
        let mut rows = parse_master(&text).unwrap();
        let mut args = test_args(dir.path());
        args.line = 40;
        args.since = Some("tag".into());
        let result = demote_alts(&mut rows, &positions.0, &book, &schema, &args).unwrap();
        assert_eq!(result.total(), 0);
        let (changes, _) =
            plan_replacements(&rows, &BTreeSet::new(), &findings, &Schema::default(), "tag", true)
                .unwrap();
        rewrite_rows(&mut rows, &changes);
        let result = demote_alts(&mut rows, &positions.0, &book, &schema, &args).unwrap();
        assert_eq!(result.protected, 0);
        assert_eq!(rows[0].get("role"), "alt");
        let result =
            demote_alts(&mut rows, &positions.0, &book, &Schema::default(), &args).unwrap();
        assert_eq!(result.total(), 0);
        assert_eq!(rows[0].get("role"), "alt");
        assert_eq!(rows[1].get("role"), "main");
    }

    #[test]
    fn run_preserves_replaced_main_as_alt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        std::fs::write(&path, replacement_fixture()).unwrap();
        let mut args = test_args(dir.path());
        args.apply = true;
        args.demote_alts = true;
        args.since = Some("tag".into());
        args.line = 40;
        run(args).unwrap();
        let rows = parse_master(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(rows[0].get("role"), "alt");
        assert_eq!(rows[1].get("role"), "main");
    }

    #[test]
    fn run_demotions_protect_any_curated_row_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        for curated in 0..3 {
            for replace_curated in [false, true] {
                let text = master(&[
                    (ROOT, "5i6h", "main", "100"),
                    (ROOT, "5i4h", "alt", "0"),
                    (ROOT, "5i5h", "record", "-999"),
                ]);
                let mut rows = parse_master(&text).unwrap();
                rows[curated].set("tier", "curated".into());
                if curated == 1 {
                    rows[0].set("tier", "curated".into());
                }
                let text = format_master(&rows);
                std::fs::write(&path, &text).unwrap();
                let mut args = test_args(dir.path());
                args.replace_curated = replace_curated;
                args.apply = true;
                args.demote_alts = true;
                args.since = Some("tag".into());
                let positions = parse_master_rows(text.as_bytes()).unwrap();
                let (_, _, book) = audit(&positions, DrawValues::both(0), 1, false).unwrap();
                let result =
                    demote_alts(&mut rows, &positions.0, &book, &Schema::default(), &args).unwrap();
                assert_eq!(result.skipped_curated, usize::from(!replace_curated));
                run(args).unwrap();
                let updated = parse_master(&std::fs::read_to_string(&path).unwrap()).unwrap();
                assert_eq!(updated, rows);
                assert_eq!(updated[1].get("role"), if replace_curated { "record" } else { "alt" });
                if !replace_curated {
                    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
                }
            }
        }
    }

    #[test]
    fn demotions_ignore_eval_only_alts_as_reference() {
        let dir = tempfile::tempdir().unwrap();
        for main in ["0", ""] {
            for child in [false, true] {
                let mut input = vec![
                    (ROOT, "5i6h", "main", main),
                    (ROOT, "5i5h", "alt", "100"),
                    (ROOT, "5i4h", "alt", "50"),
                ];
                if child {
                    input.push((A, "5a5b", "main", "-50"));
                }
                let text = master(&input);
                let positions = parse_master_rows(text.as_bytes()).unwrap();
                let (_, _, book) = audit(&positions, DrawValues::both(0), 40, false).unwrap();
                let mut rows = parse_master(&text).unwrap();
                let original = rows.clone();
                let mut args = test_args(dir.path());
                args.line = 40;
                let result =
                    demote_alts(&mut rows, &positions.0, &book, &Schema::default(), &args).unwrap();
                assert_eq!(result.total(), 0);
                assert_eq!(rows, original);
            }
        }
    }

    #[test]
    fn demotions_keep_protected_alt_sources() {
        let dir = tempfile::tempdir().unwrap();
        super::super::schema::test_schema(dir.path());
        std::fs::write(
            dir.path().join("sources.tsv"),
            "source\tdescription\treplaceable\nread\t保護\t0\n",
        )
        .unwrap();
        let schema = Schema::load(Some(dir.path())).unwrap();
        let text = master(&[(ROOT, "5i6h", "main", "100"), (ROOT, "5i4h", "alt", "0")]);
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let (_, _, book) = audit(&positions, DrawValues::both(0), 40, false).unwrap();
        let mut rows = parse_master(&text).unwrap();
        let result =
            demote_alts(&mut rows, &positions.0, &book, &schema, &test_args(dir.path())).unwrap();
        assert_eq!(result.protected, 1);
        assert_eq!(result.total(), 0);
        assert_eq!(rows[1].get("role"), "alt");
    }

    #[test]
    fn demote_alts_and_fill_replies_are_rejected_even_in_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        for apply in [false, true] {
            let mut args = test_args(dir.path());
            args.apply = apply;
            args.since = Some("tag".into());
            args.demote_alts = true;
            args.fill_replies = true;
            assert!(run(args).unwrap_err().to_string().contains("--fill-replies"));
            assert!(!dir.path().join("audit.tsv").exists());
        }
    }

    #[test]
    fn demote_only_apply_keeps_dry_run_and_outputs_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let text = master(&[
            (ROOT, "5i6h", "main", "100"),
            (ROOT, "5i4h", "alt", "60"),
            ("4k4/9/9/9/9/9/9/9/4K4 w -", "5a6b", "main", "100"),
            ("4k4/9/9/9/9/9/9/9/4K4 w -", "5a4b", "alt", "60"),
        ]);
        let path = dir.path().join("moves.tsv");
        std::fs::write(&path, &text).unwrap();
        let mut args = test_args(dir.path());
        args.demote_alts = true;
        args.line = 40;
        run(args).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let (_, _, book) = audit(&positions, DrawValues::both(0), 40, false).unwrap();
        let mut rows = parse_master(&text).unwrap();
        let mut args = test_args(dir.path());
        args.demote_alts = true;
        args.line = 40;
        args.apply = true;
        args.since = Some("tag".into());
        args.out_db = Some(dir.path().join("audited.db"));
        let summary =
            demote_alts(&mut rows, &positions.0, &book, &Schema::default(), &args).unwrap();
        assert_eq!(summary.gap, [1, 1]);
        run(args).unwrap();
        let updated = parse_master(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(updated, rows);
        let db = std::fs::read_to_string(dir.path().join("audited.db")).unwrap();
        assert!(db.contains("5i4h none 60"));
        assert!(db.contains("5a4b none 60"));
    }

    #[test]
    fn schema_outputs_and_fill_replies_apply_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        super::super::schema::test_schema(dir.path());
        std::fs::write(
            dir.path().join("sources.tsv"),
            "source\tdescription\treplaceable\nread\t読み\t1\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("moves.tsv"), replacement_fixture()).unwrap();
        let mut args = test_args(dir.path());
        args.schema = Some(dir.path().into());
        run(args).unwrap();
        for path in super::super::schema::paths(dir.path()) {
            let original = std::fs::read(&path).unwrap();
            for output in 0..4 {
                let mut args = test_args(dir.path());
                args.schema = Some(dir.path().into());
                match output {
                    0 => args.table = path.clone(),
                    1 => args.out_db = Some(path.clone()),
                    2 => args.refutation_table = Some(path.clone()),
                    _ => args.enqueue_out = Some(path.clone()),
                }
                let error = run(args).unwrap_err();
                assert!(format!("{error:#}").contains("同じです"), "{error:#}");
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
        }
        let mut args = test_args(dir.path());
        args.apply = true;
        args.since = Some("tag".into());
        args.fill_replies = true;
        assert!(run(args).unwrap_err().to_string().contains("--fill-replies"));
        for apply in [false, true] {
            let mut args = test_args(dir.path());
            args.apply = apply;
            args.since = Some("tag".into());
            args.schema = Some(dir.path().into());
            let invalid = replacement_fixture().replace("\tread\t", "\tunregistered\t");
            std::fs::write(&args.master, &invalid).unwrap();
            assert!(run(args).is_err());
            assert_eq!(std::fs::read_to_string(dir.path().join("moves.tsv")).unwrap(), invalid);
        }
    }

    #[test]
    fn run_writes_outputs_and_rejects_colliding_paths() {
        let dir = tempfile::tempdir().unwrap();
        let master_path = dir.path().join("moves.tsv");
        let text = master(&[
            (ROOT, "5i4h", "main", ""),
            (ROOT, "5i6h", "alt", "10"),
            (A, "5a5b", "main", "30"),
        ]);
        std::fs::write(&master_path, &text).unwrap();
        let args = |table: &str, out_db: Option<&str>| AuditArgs {
            table: dir.path().join(table),
            out_db: out_db.map(|p| dir.path().join(p)),
            ..test_args(dir.path())
        };
        assert!(run(args("moves.tsv", None)).is_err());
        assert!(run(args("audit.tsv", Some("audit.tsv"))).is_err());
        assert!(run(args("audit.tsv", Some("moves.tsv"))).is_err());
        assert!(run(args("fresh/audit.tsv", Some("fresh/../fresh/audit.tsv"))).is_err());
        assert_eq!(std::fs::read_to_string(&master_path).unwrap(), text);

        run(args("audit.tsv", Some("audited.db"))).unwrap();
        let table = std::fs::read_to_string(dir.path().join("audit.tsv")).unwrap();
        assert_eq!(table.lines().count(), 2);
        let db = std::fs::read_to_string(dir.path().join("audited.db")).unwrap();
        assert!(db.contains("5i4h none -30 1 2"), "{db}");
        assert!(db.contains("5i6h none 10 1 1"), "{db}");

        let mut with_refutation = args("audit2.tsv", None);
        with_refutation.refutation_table = Some(dir.path().join("refutation.tsv"));
        with_refutation.enqueue_out = Some(dir.path().join("audit2.tsv"));
        assert!(run(with_refutation).is_err());
        let mut opp_freq_only = args("audit2.tsv", None);
        opp_freq_only.weight.opp_freq = Some(dir.path().join("freq.db"));
        assert!(run(opp_freq_only).is_err());
        let mut with_refutation = args("audit2.tsv", None);
        with_refutation.refutation_table = Some(dir.path().join("refutation.tsv"));
        with_refutation.enqueue_out = Some(dir.path().join("queue.jsonl"));
        run(with_refutation).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("audit2.tsv")).unwrap(), table);
        let refutation = std::fs::read_to_string(dir.path().join("refutation.tsv")).unwrap();
        assert_eq!(refutation.lines().count(), 2);
        let queue = std::fs::read_to_string(dir.path().join("queue.jsonl")).unwrap();
        assert_eq!(queue.lines().count(), 1);

        let text = replacement_fixture();
        std::fs::write(&master_path, &text).unwrap();
        run(args("audit.tsv", None)).unwrap();
        assert_eq!(std::fs::read_to_string(&master_path).unwrap(), text);
        let mut apply = args("audit.tsv", None);
        apply.apply = true;
        assert!(run(apply).is_err());
        for (table, out_db) in [("moves.tsv", None), ("audit.tsv", Some("moves.tsv"))] {
            let mut apply = args(table, out_db);
            apply.apply = true;
            apply.since = Some("tag".into());
            assert!(run(apply).is_err());
            assert_eq!(std::fs::read_to_string(&master_path).unwrap(), text);
        }
        let mut apply = args("audit.tsv", None);
        apply.apply = true;
        apply.since = Some("tag".into());
        run(apply).unwrap();
        let updated = std::fs::read_to_string(&master_path).unwrap();
        assert_ne!(updated, text);
        assert_eq!(master_table::read_master(&master_path).unwrap().0[ROOT].moves[0].mv, "5i4h");
    }
}
