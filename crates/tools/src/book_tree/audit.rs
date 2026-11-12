//! 定跡の元データの読みの値を `book_backprop` の後退解析で伝播し、main が伝播後の 1 位から
//! どれだけ負けるかを点検し、指定時には main を置き換える。

use super::deep::{Continuation, DeepArgs, ReplyFrequency};
use super::master_table::{
    self, BookMove, MasterPosition, Row, format_master, parse_master, positions_from_rows,
};
use super::refutation::{self, RefutationOutputs, SideWeights};
use super::rejoin::WeightArgs;
use super::reply_graph::apply_usi;
use super::schema::Schema;
use super::switch::{self, SingleQueueArgs, SingleRead, SwitchRule, from_child};
use super::targets::{
    DEFAULT_PRIORITY, Line, MAX_DEPTH, MAX_PRIORITY, Opts, parse_root, read_checked_lines,
};
use super::unpromotion;
use crate::book_backprop::{
    BackpropOptions, DrawValues, Graph, MergeMode, MoveValue, PropagationStats, delta_buckets,
    evaluate_edges, graph_from_entries, parse_row, percentile, propagate_values, retain_known,
    write_book_atomic,
};
use crate::book_db::{position_from_sfen, static_position_from_sfen, strip_ply};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use rayon::prelude::*;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use sha2::{Digest, Sha256};
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
    /// 悪い alt を bench (控え) にする (--apply なしでは件数だけ確認)
    #[arg(long)]
    demote_alts: bool,
    /// 値が戻った bench (控え) を alt に戻す (--apply なしでは件数だけ確認)
    #[arg(long, conflicts_with = "fill_replies")]
    revive_bench: bool,
    /// alt を降格する値の上限 (手番側からの値)
    #[arg(long, default_value_t = -150, allow_hyphen_values = true)]
    bad_eval: i32,
    /// 置換の note に付けるタグ (--apply 時は必須)
    #[arg(long)]
    since: Option<String>,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// 点検の表 (TSV) の出力先
    #[arg(long)]
    table: PathBuf,
    /// 点検の線。main が伝播後の 1 位からこの値以上負ける局面を表に出す。--deep-plies 指定時は線未満も対象にする。
    /// --switch-rule propagated では要約の区切りと --bridge-table の線だけに使う
    #[arg(long, default_value_t = 40)]
    line: i32,
    /// main を替える局面の決め方。propagated は main と 1 位の値がどちらも手の先の局面から来ていれば差が 1 でも
    /// 替え、片方が 1 回の読みの値なら替えない (--single-enqueue-out でその手を伸ばす木を出す)
    #[arg(long, value_enum, default_value_t = SwitchRule::Line)]
    switch_rule: SwitchRule,
    #[command(flatten)]
    single: SingleQueueArgs,
    /// 両方の手の先で元データの main が N 手以上続けば、正の差が --line 未満でも対象にする (--switch-rule line だけ)
    #[arg(long, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..))]
    deep_plies: Option<usize>,
    #[command(flatten)]
    deep: DeepArgs,
    /// alt の降格・bench の復帰を判定する基準値との差
    #[arg(long, default_value_t = 100)]
    demote_line: i32,
    /// 先手を自分として解くときの千日手の値 (対局の DrawValueBlack に揃える)
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    draw_value_black: i32,
    /// 後手を自分として解くときの千日手の値 (対局の DrawValueWhite に揃える)
    #[arg(long, default_value_t = 70, allow_hyphen_values = true)]
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
    /// 読みの値のある手を指した後の、元データに無い相手の手番の局面を補う (--fill-replies とは同時に指定しない)。補った局面には、
    /// その手の読みの値を「定跡に無い応手」の値として残すので、手の値は読みの値と定跡の応手の先の値の悪い方になる
    #[arg(long, conflicts_with = "fill_replies")]
    bridge_replies: bool,
    /// 相手の手番では、読みの値のある record も伝播に使う。record だけの相手の手番の局面には、着く手の読みの値を
    /// --bridge-replies と同じ形で残すので、手の値は読みの値と record の先の値の悪い方になる
    #[arg(long)]
    opp_records: bool,
    /// --bridge-replies で main の値が補わないときより --line 以上悪くなった局面の表 (TSV) の出力先
    #[arg(long, requires = "bridge_replies")]
    bridge_table: Option<PathBuf>,
    /// bridge の局面を根にした tree enqueue --from の行 (JSON Lines)
    #[arg(long, requires = "bridge_replies")]
    bridge_enqueue_out: Option<PathBuf>,
    #[arg(long, default_value_t = 6, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(0..=MAX_DEPTH as u64))]
    bridge_depth: usize,
    /// キューの行数の上限 (表の順で先から)
    #[arg(long)]
    bridge_max: Option<usize>,
    #[arg(long, default_value_t = DEFAULT_PRIORITY, value_parser = clap::value_parser!(u32).range(0..=i64::from(MAX_PRIORITY - 99)))]
    bridge_priority: u32,
    #[arg(long)]
    bridge_opp_n: Option<usize>,
    #[arg(long, allow_hyphen_values = true)]
    bridge_stop: Option<i32>,
    #[arg(long)]
    bridge_rounds: Option<usize>,
    #[arg(long, allow_hyphen_values = true)]
    bridge_draw_black: Option<i32>,
    #[arg(long, allow_hyphen_values = true)]
    bridge_draw_white: Option<i32>,
    /// 指定順に読み、id ごとの最後の行を使う
    #[arg(long)]
    bridge_exclude_queue: Vec<PathBuf>,
    #[arg(long, default_value_t = 6)]
    bridge_exclude_min_depth: usize,
    #[command(flatten)]
    weight: WeightArgs,
    /// スレッド数 (0 = 全コア)。結果はスレッド数によらない
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

impl AuditArgs {
    fn bridge_opts(&self) -> Opts {
        Opts {
            opp_n: self.bridge_opp_n,
            stop: self.bridge_stop,
            rounds: self.bridge_rounds,
            draw_black: self.bridge_draw_black,
            draw_white: self.bridge_draw_white,
            ..Opts::default()
        }
    }
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
    pub opponent_record_positions: Option<usize>,
    pub opponent_record_moves: usize,
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
    /// 両方の先が指定手数以上続くため、線未満でも対象にした局面の数。
    pub deep_below_line: BySide,
    pub unknown_main: BySide,
    /// main の値が分からない局面のうち、main・alt が 2 手以上の局面の数。
    pub unknown_main_multi: BySide,
    pub nontrivial_sccs: usize,
    pub max_scc_size: usize,
    /// 補った局面のうち、着く手の読みの値を残した局面の数 (`--bridge-replies`)。
    pub bridged_positions: usize,
    /// 伝播のために補った、元データに無い相手の手番の局面の数。
    pub reply_positions: usize,
}

pub(super) fn run(args: AuditArgs) -> Result<()> {
    run_with_revivals(args).map(|_| ())
}

fn run_with_revivals(args: AuditArgs) -> Result<Revivals> {
    super::reply_graph::thread_pool(args.threads)?.install(|| run_in_pool(args))
}

fn run_in_pool(args: AuditArgs) -> Result<Revivals> {
    ensure!(!args.apply || args.since.is_some(), "--apply には --since が必要です");
    if let Some(since) = &args.since {
        ensure!(!since.trim().is_empty(), "--since を指定してください");
        ensure!(!since.contains(['\t', '\r', '\n']), "since にタブや改行は使えません");
    }
    let fill = if args.bridge_replies {
        Fill::Bridge
    } else if args.fill_replies {
        Fill::Unread
    } else {
        Fill::Off
    };
    ensure!(
        !(args.apply && fill == Fill::Unread),
        "--apply と --fill-replies は併用できません"
    );
    ensure!(
        !(args.demote_alts && fill == Fill::Unread),
        "--demote-alts と --fill-replies は併用できません"
    );
    ensure!(
        !(args.revive_bench && fill == Fill::Unread),
        "--revive-bench と --fill-replies は併用できません"
    );
    ensure!(
        (args.bridge_table.is_none() && args.bridge_enqueue_out.is_none()) || args.bridge_replies,
        "--bridge-table / --bridge-enqueue-out は --bridge-replies と一緒に使います"
    );
    let propagated_rule = args.switch_rule == SwitchRule::Propagated;
    ensure!(
        !propagated_rule || args.deep_plies.is_none(),
        "--deep-plies は --switch-rule line と一緒に使います"
    );
    ensure!(
        propagated_rule || args.single.single_enqueue_out.is_none(),
        "--single-enqueue-out は --switch-rule propagated と一緒に使います"
    );
    if args.single.single_enqueue_out.is_some() {
        args.single.check()?;
    }
    let schema = Schema::load(args.schema.as_deref())?;
    ensure!(args.demote_line > 0, "--demote-line は 1 以上にしてください");
    ensure!(args.line > 0, "--line は 1 以上にしてください");
    ensure!(args.refutation_margin > 0, "--refutation-margin は 1 以上にしてください");
    let refutation = args.refutation_table.is_some() || args.enqueue_out.is_some();
    let bridge_output = args.bridge_table.is_some() || args.bridge_enqueue_out.is_some();
    let single_output = args.single.single_enqueue_out.is_some();
    ensure!(
        refutation || bridge_output || single_output || args.weight.opp_freq.is_none(),
        "--opp-freq は --refutation-table / --enqueue-out / --bridge-table / --bridge-enqueue-out / --single-enqueue-out と一緒に使います"
    );
    args.bridge_opts().check()?;
    ensure!(
        args.deep_plies.is_some() || args.deep.deep_opp_freq.is_none(),
        "--deep-opp-freq は --deep-plies と一緒に使います"
    );
    args.weight.check()?;
    let mut inputs = vec![("--master", args.master.as_path())];
    for path in &args.bridge_exclude_queue {
        if args.apply {
            reject_same_file(&args.master, &[("--bridge-exclude-queue", path.as_path())])?;
        }
        inputs.push(("--bridge-exclude-queue", path.as_path()));
    }
    for path in &args.single.exclude_queue {
        if args.apply {
            reject_same_file(&args.master, &[("--exclude-queue", path.as_path())])?;
        }
        inputs.push(("--exclude-queue", path.as_path()));
    }
    for path in &args.single.walk_table {
        if args.apply {
            reject_same_file(&args.master, &[("--walk-table", path.as_path())])?;
        }
        inputs.push(("--walk-table", path.as_path()));
    }
    inputs.extend(args.weight.inputs());
    if let Some(freq) = &args.deep.deep_opp_freq {
        inputs.push(("--deep-opp-freq", freq.as_path()));
        if args.apply {
            reject_same_file(&args.master, &[("--deep-opp-freq", freq.as_path())])?;
        }
    }
    let deep_freq = args
        .deep
        .deep_opp_freq
        .as_deref()
        .map(|path| ReplyFrequency::read(path, args.deep.deep_min_games))
        .transpose()?;
    let schema_paths = args.schema.as_deref().map(super::schema::paths);
    inputs.extend(schema_paths.iter().flatten().map(|p| ("--schema", p.as_path())));
    let outputs: Vec<(&str, &Path)> = [
        ("--table", Some(args.table.as_path())),
        ("--out-db", args.out_db.as_deref()),
        ("--refutation-table", args.refutation_table.as_deref()),
        ("--enqueue-out", args.enqueue_out.as_deref()),
        ("--bridge-table", args.bridge_table.as_deref()),
        ("--bridge-enqueue-out", args.bridge_enqueue_out.as_deref()),
        ("--single-enqueue-out", args.single.single_enqueue_out.as_deref()),
    ]
    .into_iter()
    .filter_map(|(name, path)| Some((name, path?)))
    .collect();
    for (i, (name, path)) in outputs.iter().enumerate() {
        let mut others = inputs.clone();
        others.extend(outputs[..i].iter().copied());
        reject_same_file(path, &others).with_context(|| format!("{name} の出力先"))?;
    }
    let full_rows = args.apply || args.demote_alts || args.revive_bench;
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
            if !master_table::is_unplayed(row.get("role"))
                || (args.opp_records && row.get("role") == "record")
            {
                rows.push(row.clone());
            } else if row.get("tier") == "curated" {
                // 指さない行 (record・bench) の列を保持せず、置換を保護する局面だけを覚える。
                curated_records.insert(row.get("key").to_owned());
            }
            Ok((at, row))
        }))?;
        drop(seen);
        curated_records.retain(|key| positions.0.contains_key(key));
        master_table::validate(&rows)?;
        (Vec::new(), rows, positions, curated_records)
    };
    let unpromoted = unpromotion::demote_rows(&mut rows, args.since.as_deref().unwrap_or("確認"))?;
    let positions = if unpromoted.total() > 0 {
        master_table::validate(&rows)?;
        let (mut changed, records) = positions;
        for key in &unpromoted.keys {
            changed.remove(key);
        }
        let rebuilt = positions_from_rows(
            rows.iter()
                .enumerate()
                .filter(|(_, row)| unpromoted.keys.contains(row.get("key")))
                .map(|(i, row)| Ok((i + 2, row.clone()))),
        )?;
        changed.extend(rebuilt.0);
        (changed, records + unpromoted.total())
    } else {
        positions
    };
    eprintln!("{}", unpromoted.format());
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let records = args.opp_records.then(|| record_positions(&rows)).transpose()?;
    let (findings, summary, book) = audit_select(
        &positions,
        draws,
        Select {
            line: args.line,
            rule: args.switch_rule,
            deep_plies: args.deep_plies,
            deep_freq: deep_freq.as_ref(),
        },
        fill,
        records.as_ref(),
    )?;
    let (changes, replacements) = plan_replacements_with(
        &rows,
        &curated_records,
        &findings,
        &schema,
        args.since.as_deref().unwrap_or("確認"),
        args.replace_curated,
        args.switch_rule,
    )?;
    let weights = refutation::side_weights(&positions.0, &args.weight)?;
    let single = if single_output {
        let mut reads = single_reads(&findings);
        args.single.rank(&mut reads, weights.as_ref(), args.weight.min_weight)?;
        let (lines, excluded) = switch::queue(&reads, &args.single)?;
        Some((refutation::format_queue(&lines)?, lines.len(), excluded))
    } else {
        None
    };
    // 除外キューの読み込みや行の検査で失敗したときに表だけが更新されないよう、bridge の出力は書く前に全部作る。
    let bridge = if bridge_output {
        let mut worse = bridge_findings(&positions.0, draws, &book, args.line, records.as_ref())?;
        weight_bridge_findings(&mut worse, weights.as_ref(), args.weight.min_weight);
        let table = args.bridge_table.as_ref().map(|_| format_bridge_table(&worse));
        let queue = match &args.bridge_enqueue_out {
            Some(_) => {
                let (lines, excluded) = bridge_queue(&worse, &args)?;
                Some((refutation::format_queue(&lines)?, lines.len(), excluded))
            }
            None => None,
        };
        Some((worse.len(), table, queue))
    } else {
        None
    };
    write_atomic(&args.table, &format_table(&findings))?;
    if let (Some(path), Some((text, queued, excluded))) = (&args.single.single_enqueue_out, single)
    {
        write_atomic(path, &text)?;
        println!(
            "1 回の読みの手を伸ばす木のキューの行: {queued} (既存の木で除いた局面 {excluded})"
        );
    }
    if let Some((found, table, queue)) = bridge {
        if let (Some(path), Some(text)) = (&args.bridge_table, table) {
            write_atomic(path, &text)?;
        }
        if let (Some(path), Some((text, queued, excluded))) = (&args.bridge_enqueue_out, queue) {
            write_atomic(path, &text)?;
            println!("補った切れ目のキューの行: {queued} (除外 {excluded})");
        }
        println!("補って main の値が {} 以上悪くなった局面: {found}", args.line);
    }
    if let Some(out) = &args.out_db {
        write_audited_db(out, &positions.0, &book)?;
    }
    print!("{}", format_summary(&summary, args.line, draws, args.deep_plies));
    if let Some(path) = &args.deep.deep_opp_freq {
        println!("継続判定の相手頻度定跡: {}", path.display());
    }
    if refutation {
        let outputs = RefutationOutputs {
            table: args.refutation_table.as_deref(),
            enqueue_out: args.enqueue_out.as_deref(),
            margin: args.refutation_margin,
            depth: args.refutation_depth,
            max: args.refutation_max,
            priority: args.priority,
            weights: weights.as_ref(),
            min_weight: args.weight.min_weight,
        };
        print!("{}", refutation::write_outputs(&rows, &positions.0, &book, &outputs)?);
    }
    drop(weights);
    rewrite_rows(&mut rows, &changes);
    let demotions = if args.demote_alts {
        Some(demote_alts(&mut rows, &positions.0, &book, &schema, &args)?)
    } else {
        None
    };
    let revivals = if args.revive_bench {
        revive_bench(&mut rows, &positions.0, &book, &schema, &args)?
    } else {
        Revivals::default()
    };
    master_table::validate(&rows)?;
    if args.apply
        && (!changes.is_empty()
            || unpromoted.total() > 0
            || demotions.as_ref().is_some_and(|d| d.total() > 0)
            || revivals.revived.iter().sum::<usize>() > 0)
    {
        write_master(&args.master, &original, &rows)?;
    }
    if args.demote_alts || args.revive_bench {
        println!("控えの判定: demote-line {} / bad-eval {}", args.demote_line, args.bad_eval);
    }
    if args.revive_bench {
        println!(
            "bench から alt に戻す手: 先手番 {}・後手番 {} / curated の main が無い控え {}",
            revivals.revived[0], revivals.revived[1], revivals.curated_without_main
        );
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
    Ok(revivals)
}

#[derive(Debug, Default, PartialEq)]
struct ReplacementSummary {
    replaced: BySide,
    curated: usize,
    skipped_curated: usize,
    protected: usize,
    eval: usize,
    /// --switch-rule propagated で、main の値が 1 回の読みなので替えなかった局面。
    main_eval: Option<usize>,
    missing: usize,
}

impl ReplacementSummary {
    fn format(&self) -> String {
        let eval = match self.main_eval {
            None => format!("1 位が伸ばしていない手なので替えなかった局面 {}", self.eval),
            Some(n) => format!(
                "1 位の値が 1 回の読みなので替えなかった局面 {}, main の値が 1 回の読みなので替えなかった局面 {n}",
                self.eval
            ),
        };
        format!(
            "置換する局面: 先手番 {}・後手番 {} (うち curated {}), curated なので替えなかった局面 {}, source が置換対象でない局面 {}, {eval}, 対象行が無いか main・alt でない局面 {}",
            self.replaced[0],
            self.replaced[1],
            self.curated,
            self.skipped_curated,
            self.protected,
            self.missing
        )
    }
}

type Changes = BTreeMap<(String, String), Row>;

/// 点検の表のうち、main か 1 位の値が 1 回の読みの局面 (--switch-rule propagated では替えない局面)。
fn single_reads(findings: &[Finding]) -> Vec<SingleRead> {
    findings
        .iter()
        .filter_map(|f| {
            let moves: Vec<String> = [&f.main, &f.best]
                .into_iter()
                .filter(|m| m.origin == Origin::Eval)
                .map(|m| m.mv.clone())
                .collect();
            (!moves.is_empty()).then(|| SingleRead {
                key: f.key.clone(),
                ply: f.ply.clone(),
                main: f.main.mv.clone(),
                main_value: f.main.value,
                best: f.best.mv.clone(),
                best_value: f.best.value,
                moves,
                weight: None,
            })
        })
        .collect()
}

#[cfg(test)]
fn plan_replacements(
    rows: &[Row],
    curated_records: &BTreeSet<String>,
    findings: &[Finding],
    schema: &Schema,
    since: &str,
    replace_curated: bool,
) -> Result<(Changes, ReplacementSummary)> {
    plan_replacements_with(
        rows,
        curated_records,
        findings,
        schema,
        since,
        replace_curated,
        SwitchRule::Line,
    )
}

fn plan_replacements_with(
    rows: &[Row],
    curated_records: &BTreeSet<String>,
    findings: &[Finding],
    schema: &Schema,
    since: &str,
    replace_curated: bool,
    rule: SwitchRule,
) -> Result<(Changes, ReplacementSummary)> {
    let selected: BTreeMap<_, _> =
        rows.iter().map(|row| ((row.get("key"), row.get("move")), row)).collect();
    let curated_keys: BTreeSet<_> = rows
        .iter()
        .filter(|row| row.get("tier") == "curated")
        .map(|row| row.get("key"))
        .collect();
    let mut changes = BTreeMap::new();
    let mut summary = ReplacementSummary {
        main_eval: (rule == SwitchRule::Propagated).then_some(0),
        ..ReplacementSummary::default()
    };
    for f in findings {
        if f.best.origin == Origin::Eval {
            summary.eval += 1;
            continue;
        }
        if let Some(count) = summary.main_eval.as_mut()
            && f.main.origin == Origin::Eval
        {
            *count += 1;
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
            "bench (控え) に降格する alt: 先手番 {}・後手番 {} / 理由 (a) 値: 先手番 {}・後手番 {} / 理由 (b) 差: 先手番 {}・後手番 {} / 値が不明 {} / source が保護対象 {} / curated なので降格しなかった局面 {}",
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
        let moves = audited_moves(book.graph_for(key), key, position, known)?;
        let values: BTreeMap<_, _> = moves
            .iter()
            .flatten()
            .map(|m| (m.mv.as_str(), i64::from(m.value) + i64::from(m.tie_lowered)))
            .collect();
        let best = reference_best(&moves, position);
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
            let by_gap = gap >= i64::from(args.demote_line);
            let by_bad = value <= i64::from(args.bad_eval) && value < best;
            if !by_gap && !by_bad {
                continue;
            }
            if !schema.replaceable(row.get("source")) {
                summary.protected += 1;
                continue;
            }
            row.set("role", "bench".into());
            let note = row.get("note");
            let since = args.since.as_deref().unwrap_or("確認");
            row.set("note", format!("{note}{}{since}: 伝播後の値が悪いので alt から bench (控え) (基準との差 {gap} / 値 {value} / demote-line {} / bad-eval {})",
                if note.is_empty() { "" } else { "。" }, args.demote_line, args.bad_eval));
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

fn reference_best(moves: &[Option<AuditedMove>], position: &MasterPosition) -> Option<i64> {
    moves
        .iter()
        .flatten()
        .filter(|m| m.origin == Origin::Propagated || m.mv == position.moves[0].mv)
        .map(|m| i64::from(m.value) + i64::from(m.tie_lowered))
        .max()
}

#[derive(Default, Debug)]
struct Revivals {
    revived: BySide,
    curated_without_main: usize,
}

fn revive_bench(
    rows: &mut [Row],
    positions: &BTreeMap<String, MasterPosition>,
    book: &AuditedBook,
    schema: &Schema,
    args: &AuditArgs,
) -> Result<Revivals> {
    let curated: BTreeSet<String> = rows
        .iter()
        .filter(|r| r.get("tier") == "curated")
        .map(|r| r.get("key").to_owned())
        .collect();
    let mut references = BTreeMap::new();
    for ((key, position), known) in positions.iter().zip(&book.known) {
        let moves = audited_moves(book.graph_for(key), key, position, known)?;
        let best = reference_best(&moves, position);
        if let Some(best) = best {
            references.insert(key.as_str(), best);
        }
    }
    let curated_mains: BTreeSet<_> = rows
        .iter()
        .filter(|r| r.get("tier") == "curated" && r.get("role") == "main")
        .map(|r| r.get("key").to_owned())
        .collect();
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let mut values = vec![None; rows.len()];
    for side in ["b", "w"] {
        let graph = if side == "b" {
            &book.graph
        } else {
            book.white_graph.as_ref().unwrap_or(&book.graph)
        };
        let mut indices = Vec::new();
        let mut edges = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if row.get("role") == "bench"
                && row.get("key").split_whitespace().nth(1) == Some(side)
                && let Some(target) = bench_target(row, graph)?
            {
                indices.push(i);
                edges.push(target);
            }
        }
        for (i, value) in indices.into_iter().zip(evaluate_edges(graph, draws, &edges)?) {
            values[i] = Some(value);
        }
    }
    let mut summary = Revivals::default();
    for (row, propagated) in rows.iter_mut().zip(values) {
        if row.get("role") != "bench"
            || !schema.replaceable(row.get("source"))
            || (!args.replace_curated && curated.contains(row.get("key")))
        {
            continue;
        }
        if row.get("tier") == "curated" && !curated_mains.contains(row.get("key")) {
            summary.curated_without_main += 1;
            continue;
        }
        // 開始時に指せた手は復帰対象にしない。降格と復帰を同じ実行で往復させない。
        if positions
            .get(row.get("key"))
            .is_some_and(|p| p.moves.iter().any(|m| m.mv == row.get("move")))
        {
            continue;
        }
        let Some(&best) = references.get(row.get("key")) else {
            continue;
        };
        let Some(value) = propagated
            .map(i64::from)
            .or(master_table::mean_eval(row.get("evals"))?.map(i64::from))
        else {
            continue;
        };
        if value <= i64::from(args.bad_eval) || best - value >= i64::from(args.demote_line) {
            continue;
        }
        if unpromotion::is_needless(row.get("key"), row.get("move"))? {
            continue;
        }
        row.set("role", "alt".into());
        let note = row.get("note");
        let since = args.since.as_deref().unwrap_or("確認");
        row.set(
            "note",
            format!(
                "{note}{}{since}: 値が戻ったので bench から alt (基準との差 {} / 値 {value} / demote-line {} / bad-eval {})",
                if note.is_empty() { "" } else { "。" }, best - value, args.demote_line, args.bad_eval
            ),
        );
        schema.validate_row(row)?;
        summary.revived[usize::from(row.get("key").split_whitespace().nth(1) == Some("w"))] += 1;
    }
    Ok(summary)
}

fn bench_target(row: &Row, graph: &Graph) -> Result<Option<(usize, bool)>> {
    let mut pos = position_from_sfen(&format!("{} 1", row.get("key")))?;
    if apply_usi(&mut pos, row.get("move")).is_err() {
        return Ok(None);
    }
    let child = pos.to_sfen();
    // 反転の辺を張らないグラフ (--bridge-replies) では、伝播と同じく反転した局面を使わない。
    let node = graph.find(strip_ply(&child)).or_else(|| {
        if graph.options.skip_flipped_children {
            return None;
        }
        rshogi_book::flipped_key(&child).and_then(|key| graph.find(strip_ply(&key)))
    });
    Ok(node
        .filter(|&node| graph.moves(node).any(|m| m.usable))
        .map(|node| (node, row.get("key").split_whitespace().nth(1) == Some("b"))))
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
pub(super) fn resolve_output(path: &Path) -> Result<PathBuf> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("{} を作れません", parent.display()))?;
    }
    Ok(resolve(path))
}

/// 同じファイルかを比べるためのパス。まだ無いファイルは親ディレクトリを解決して名前を付ける。
pub(super) fn resolve(path: &Path) -> PathBuf {
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
    white_graph: Option<Graph>,
    /// 元データに無く、着く手の読みの値を最後の手として残した局面 ([`super::switch::from_child`])。
    pub capped: BTreeSet<String>,
}

impl AuditedBook {
    pub(super) fn graph_for(&self, key: &str) -> &Graph {
        if key.split_whitespace().nth(1) == Some("w") {
            self.white_graph.as_ref().unwrap_or(&self.graph)
        } else {
            &self.graph
        }
    }
}

fn write_audited_db(
    path: &Path,
    positions: &BTreeMap<String, MasterPosition>,
    book: &AuditedBook,
) -> Result<()> {
    write_book_atomic(path, |mut writer| {
        writeln!(writer, "{}", crate::book_backprop::BOOK_HEADER)?;
        for ((key, position), known) in positions.iter().zip(&book.known) {
            let graph = book.graph_for(key);
            let Some(node) = graph.find(key) else {
                continue;
            };
            writeln!(writer, "sfen {}", graph.sfen(node))?;
            let mut values = graph.moves(node);
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
    let skip_flipped = graph.options.skip_flipped_children;
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
                if !skip_flipped
                    && !edge.via_flip
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

/// 元データに無い相手の手番の局面を補うか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fill {
    /// 補わない。
    Off,
    /// 読みの値の無い手の行き先だけを補う (`--fill-replies`)。
    Unread,
    /// 読みの値のある手の行き先も補う (`--bridge-replies`)。
    Bridge,
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
    /// 補った局面のうち、読みの値を「定跡に無い応手」の値として残した局面の数。
    bridged_positions: usize,
    /// 元データに無く、着く手の読みの値を最後の手として残した局面。
    capped: BTreeSet<String>,
}

/// 元データの手を指した後の局面のうち、元データに無いもの。
enum Leaf<'a> {
    /// 補わない局面 (読みの値のある手の行き先と、行き先を反転した局面)。
    Blocked(&'a str),
    /// 補う候補と、そこへ着く手の読みの値 (手を指した側から。読みの値の無い手は `None`)。
    Candidate(&'a str, Option<i32>),
}

/// 局面の手の行き先のうち元データに無いものを `visit` に渡す。
fn visit_leaves(
    positions: &BTreeMap<String, MasterPosition>,
    key: &str,
    position: &MasterPosition,
    fill: Fill,
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
            match (m.eval, fill) {
                (Some(_), Fill::Unread) => visit(Leaf::Blocked(child_key)),
                // 読みの値の無い手の行き先は上限を付けられず甘くなるので、Bridge では補わない。
                (None, Fill::Bridge) => {}
                (eval, _) => visit(Leaf::Candidate(child_key, eval)),
            }
        }
        pos.undo_move(mv);
    }
    Ok(())
}

/// 手を指した後の、元データに無い局面 (反転した局面も無いもの) を、相手の合法手のうち元データの局面に着く手だけを
/// 持つ局面として補う。元データは自分の手番の局面にしか行を持たないことが多く、補わないと先の読みの値が相手の手番の
/// 局面で途切れる。反転した局面が元データにある局面へは応手を張らない (千日手の値が先後で違うと、反転で合流した循環を
/// 解けない)。
///
/// 補った値は定跡に無い応手を見ない分だけ甘い。`Fill::Unread` では読みの値のある手が着く局面は補わない。
/// `Fill::Bridge` では補い、着く手の読みの値 (相手の全部の応手を見た値) を、元データの局面に着かない合法手 1 つの
/// 値として残す。相手は手を指した側に悪い方を選ぶので、手の値は読みの値と定跡の応手の先の値の悪い方になり、
/// 読みの値より甘くならない。読みの値のある手が複数の局面から着くときは、手を指した側に一番悪い読みの値を残す
/// (同じ局面に読みの値の無い手からも着けば、その手にも効く。読みの値の無い手だけが着く局面は補わない)。残す先の合法手が無い局面 (全部の応手が元データの
/// 局面か、反転した局面か、補う候補の局面に着く) は補わない。
fn reply_positions(
    positions: &BTreeMap<String, MasterPosition>,
    fill: Fill,
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
                visit_leaves(positions, key, position, fill, |leaf| {
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
    // (手数の最小, 着く手の読みの値の最小 (手を指した側から))。
    let mut candidates: BTreeMap<String, (u32, Option<i32>)> = BTreeMap::new();
    for chunk in entries.chunks(CHUNK) {
        let part: Vec<Vec<(String, u32, Option<i32>)>> = chunk
            .par_iter()
            .map(|&(key, position)| -> Result<Vec<(String, u32, Option<i32>)>> {
                let ply = position.ply.parse::<u32>().map_or(1, |n| n.saturating_add(1));
                let mut out = Vec::new();
                visit_leaves(positions, key, position, fill, |leaf| {
                    if let Leaf::Candidate(k, eval) = leaf
                        && blocked.binary_search(&hash(k)).is_err()
                    {
                        out.push((k.to_owned(), ply, eval));
                    }
                })?;
                Ok(out)
            })
            .collect::<Result<_>>()?;
        for (key, ply, eval) in part.into_iter().flatten() {
            let entry = candidates.entry(key).or_insert((ply, eval));
            entry.0 = entry.0.min(ply);
            entry.1 = match (entry.1, eval) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
    }
    // 読みの値を残す手は、補う候補の局面 (とその反転) にも着かせない。着くと book_backprop がその手に辺を張り、
    // 残した値が補った局面の値で置き換わる。
    let pending: BTreeSet<String> = candidates.keys().cloned().collect();
    let in_pending = |sfen: &str| {
        pending.contains(strip_ply(sfen))
            || rshogi_book::flipped_key(sfen).is_some_and(|f| pending.contains(strip_ply(&f)))
    };
    let replies: Vec<Option<(String, MasterPosition)>> = candidates
        .into_par_iter()
        .map(|(key, (ply, cap))| -> Result<Option<(String, MasterPosition)>> {
            let mut pos = position_from_sfen(&format!("{key} 1"))?;
            let mut legal = MoveList::new();
            generate_legal_all(&pos, &mut legal);
            let mut moves = Vec::new();
            // 読みの値を残す、元データの局面にも反転した局面にも着かない合法手 (生成順の最初)。
            let mut outside = None;
            for &reply in legal.iter() {
                pos.do_move(reply, pos.gives_check(reply));
                let to = pos.to_sfen();
                if positions.contains_key(strip_ply(&to)) {
                    if !flipped_in_master(&to) {
                        moves.push(BookMove {
                            mv: reply.to_usi(),
                            eval: None,
                        });
                    }
                } else if cap.is_some()
                    && outside.is_none()
                    && !flipped_in_master(&to)
                    && !in_pending(&to)
                {
                    outside = Some(reply.to_usi());
                }
                pos.undo_move(reply);
            }
            if let Some(eval) = cap {
                let Some(mv) = outside else {
                    return Ok(None);
                };
                if moves.is_empty() {
                    return Ok(None);
                }
                moves.push(BookMove {
                    mv,
                    eval: Some(-eval),
                });
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

/// record だけで足す相手の手番の局面ごとの、読みの値の上限の手。
///
/// record だけの局面は record の手しか応手を持たないので、値が手を指した側に甘くなる。--bridge-replies と同じく、
/// 着く手の読みの値 (相手の全部の応手を見た値) のうち手を指した側に一番悪い値を、record に無く、行き先が伝播の
/// 局面に無く、行き先からどの応手も伝播の局面に着かない (--bridge-replies に補われない) 合法手 1 つ (生成順の最初)
/// の値として置き、手の値を読みの値と record の先の値の悪い方にする。
/// 読みの値のある手が着かない局面は返さない。置ける手が無い局面は `None` を返す。
fn record_caps(
    positions: &BTreeMap<String, MasterPosition>,
    records: &BTreeMap<String, MasterPosition>,
    opponent: &str,
) -> Result<BTreeMap<String, Option<BookMove>>> {
    let added = |key: &str| {
        key.split_whitespace().nth(1) == Some(opponent)
            && records.contains_key(key)
            && !positions.contains_key(key)
    };
    let mut worst: BTreeMap<String, i32> = BTreeMap::new();
    for (key, position) in positions {
        if key.split_whitespace().nth(1) == Some(opponent) {
            continue;
        }
        let mut pos = position_from_sfen(&format!("{key} 1"))?;
        for m in &position.moves {
            let Some(eval) = m.eval else {
                continue;
            };
            let Ok(mv) = apply_usi(&mut pos, &m.mv) else {
                continue;
            };
            let child = strip_ply(&pos.to_sfen()).to_owned();
            pos.undo_move(mv);
            if added(&child) {
                let entry = worst.entry(child).or_insert(eval);
                *entry = (*entry).min(eval);
            }
        }
    }
    let mut caps = BTreeMap::new();
    for (key, eval) in worst {
        let mut pos = position_from_sfen(&format!("{key} 1"))?;
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        let recorded = &records[&key].moves;
        let mut cap = None;
        for &reply in legal.iter() {
            let usi = reply.to_usi();
            if recorded.iter().any(|r| r.mv == usi) {
                continue;
            }
            pos.do_move(reply, pos.gives_check(reply));
            let to = strip_ply(&pos.to_sfen()).to_owned();
            // --bridge-replies は、読みの値のある手の行き先で、応手が伝播の局面に着く局面を補う。補われると上限の手に
            // 辺が張られて値が置き換わるので、行き先からどの応手も伝播の局面に着かない手を選ぶ。
            let leaf = !positions.contains_key(&to) && !added(&to) && {
                let mut next = MoveList::new();
                generate_legal_all(&pos, &mut next);
                !next.iter().any(|&m| {
                    pos.do_move(m, pos.gives_check(m));
                    let k = strip_ply(&pos.to_sfen()).to_owned();
                    pos.undo_move(m);
                    positions.contains_key(&k) || added(&k)
                })
            };
            pos.undo_move(reply);
            if leaf {
                cap = Some(BookMove {
                    mv: usi,
                    eval: Some(-eval),
                });
                break;
            }
        }
        caps.insert(key, cap);
    }
    Ok(caps)
}

fn record_positions(rows: &[Row]) -> Result<BTreeMap<String, MasterPosition>> {
    let mut records = BTreeMap::<String, MasterPosition>::new();
    for row in rows.iter().filter(|r| r.get("role") == "record") {
        let Some(eval) = master_table::mean_eval(row.get("evals"))? else {
            continue;
        };
        records
            .entry(row.get("key").to_owned())
            .or_insert_with(|| MasterPosition {
                ply: row.get("ply").to_owned(),
                source: String::new(),
                moves: Vec::new(),
            })
            .moves
            .push(BookMove {
                mv: row.get("move").to_owned(),
                eval: Some(eval),
            });
    }
    Ok(records)
}

#[derive(Default)]
struct RecordUsage {
    positions: usize,
    moves: usize,
}

fn propagate_for_sides(
    positions: &BTreeMap<String, MasterPosition>,
    records: Option<&BTreeMap<String, MasterPosition>>,
    draws: DrawValues,
    fill: Fill,
    skip_flipped: bool,
) -> Result<(Propagated, Option<Graph>, RecordUsage)> {
    let Some(records) = records else {
        return Ok((
            propagate_with(positions, draws, fill, skip_flipped)?,
            None,
            RecordUsage::default(),
        ));
    };
    let solve = |opponent: &str| -> Result<(Propagated, RecordUsage)> {
        let mut expanded = positions.clone();
        let caps = record_caps(positions, records, opponent)?;
        let mut capped = BTreeSet::new();
        for (key, record) in records {
            if key.split_whitespace().nth(1) != Some(opponent) {
                continue;
            }
            if let Some(position) = expanded.get_mut(key) {
                position.moves.extend(record.moves.clone());
                continue;
            }
            let mut position = record.clone();
            match caps.get(key) {
                None => {}
                Some(Some(cap)) => {
                    capped.insert(key.clone());
                    position.moves.push(cap.clone());
                }
                // 上限を置ける手が無ければ、record で伝播せず、着く手の読みの値をそのまま使わせる。
                Some(None) => continue,
            }
            expanded.insert(key.clone(), position);
        }
        // 反転した局面へ合流すると、足した record が反転先で手番の側自身の手として数えられるので辺を張らない。
        let mut result = propagate_with(&expanded, draws, fill, true)?;
        result.capped.extend(capped);
        // 追加した record は相手の応手専用なので、点検との対応には元の手だけを残す。
        let mut usage = RecordUsage::default();
        let mut illegal_records = 0;
        let mut known = Vec::with_capacity(positions.len());
        for ((key, _), mask) in expanded.iter().zip(&result.known) {
            let playable = positions.get(key).map_or(0, |p| p.moves.len());
            // 足した上限の手は record ではないので、record の手の分だけを数える。
            let added = match records.get(key) {
                Some(r) if key.split_whitespace().nth(1) == Some(opponent) => r.moves.len(),
                _ => 0,
            };
            let record_mask = || mask.iter().skip(playable).take(added);
            illegal_records += record_mask().filter(|&&k| !k).count();
            let used = record_mask().filter(|&&k| k).count();
            usage.moves += used;
            usage.positions += usize::from(used > 0);
            if positions.contains_key(key) {
                known.push(mask[..playable].to_vec());
            }
        }
        result.known = known;
        result.illegal_moves -= illegal_records;
        Ok((result, usage))
    };
    let (mut black, black_usage) = solve("w")?;
    let (white, white_usage) = solve("b")?;
    for ((key, _), (b, w)) in positions.iter().zip(black.known.iter_mut().zip(white.known)) {
        if key.split_whitespace().nth(1) == Some("w") {
            *b = w;
        }
    }
    black.stats.nontrivial_sccs += white.stats.nontrivial_sccs;
    black.stats.max_scc_size = black.stats.max_scc_size.max(white.stats.max_scc_size);
    // 同じ補完局面が両方の伝播に現れても、要約では一局面と数える。
    let mut replies = BTreeSet::new();
    let mut bridged = BTreeSet::new();
    for (graph, opponent) in [(&black.graph, "w"), (&white.graph, "b")] {
        for node in 0..graph.len() {
            let key = graph.key(node);
            if !positions.contains_key(key)
                && !(records.contains_key(key) && key.split_whitespace().nth(1) == Some(opponent))
            {
                replies.insert(key.to_owned());
                if graph.moves(node).any(|m| m.edge.is_none()) {
                    bridged.insert(key.to_owned());
                }
            }
        }
    }
    black.reply_positions = replies.len();
    black.bridged_positions = bridged.len();
    black.capped.extend(white.capped);
    Ok((
        black,
        Some(white.graph),
        RecordUsage {
            positions: black_usage.positions + white_usage.positions,
            moves: black_usage.moves + white_usage.moves,
        },
    ))
}

/// 局面の値の分かる手だけを伝播する。(伝播後のグラフ, 局面ごと・手ごとの値の分かる手, 非合法手の数, 伝播の集計)。
fn propagate_entries(
    all: &[(&str, &MasterPosition)],
    draws: DrawValues,
    skip_flipped: bool,
) -> Result<(Graph, Vec<Vec<bool>>, usize, PropagationStats)> {
    let options = BackpropOptions {
        skip_flipped_children: skip_flipped,
        ..BackpropOptions::default()
    };
    let mut graph = graph_from_entries(
        all.iter().map(|&(key, p)| {
            (key, p.ply.as_str(), p.moves.iter().map(|m| (m.mv.as_str(), m.eval)))
        }),
        options,
    )?;
    let illegal_moves = graph.illegal_moves;
    let known = known_moves(all, &graph)?;
    retain_known(&mut graph, &known)?;
    let stats = propagate_values(&mut graph, draws, MergeMode::Replace)?;
    Ok((graph, known, illegal_moves, stats))
}

/// 元データの値の分かる手だけを伝播する。`fill` に応じて元データに無い相手の手番の局面を補う
/// ([`reply_positions`])。key が正規の SFEN でなければ止まる。
fn propagate(
    positions: &BTreeMap<String, MasterPosition>,
    draws: DrawValues,
    fill: Fill,
) -> Result<Propagated> {
    propagate_with(positions, draws, fill, fill == Fill::Bridge)
}

/// [`propagate`] の本体。`skip_flipped` なら反転した局面へ辺を張らない。`Fill::Bridge` は補った辺が反転で合流する
/// 局面を千日手の値の違う循環に入れると止まる (1 行で両方の向きの値を表せない) ので、反転の辺を張らずに解く。
fn propagate_with(
    positions: &BTreeMap<String, MasterPosition>,
    draws: DrawValues,
    fill: Fill,
    skip_flipped: bool,
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
    let replies = if fill == Fill::Off {
        BTreeMap::new()
    } else {
        reply_positions(positions, fill)?
    };
    // 読みの値を残した局面は、元データの局面に着かない手 (値のある行) を持つ。
    let capped: BTreeSet<String> = replies
        .iter()
        .filter(|(_, p)| p.moves.iter().any(|m| m.eval.is_some()))
        .map(|(key, _)| key.clone())
        .collect();
    let bridged_count = capped.len();
    let mut all: Vec<(&str, &MasterPosition)> =
        positions.iter().chain(&replies).map(|(key, p)| (key.as_str(), p)).collect();
    // 補った局面は元データと key が重ならないので、key 順に並べれば元データの局面の順は保たれる。
    all.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let (graph, known_all, illegal_moves, stats) = propagate_entries(&all, draws, skip_flipped)?;
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
        bridged_positions: bridged_count,
        capped,
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
    let Propagated { graph, known, .. } = propagate(positions, draws, Fill::Off)?;
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
    let Propagated { graph, known, .. } = propagate(positions, draws, Fill::Off)?;
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

fn main_continues_after(
    positions: &BTreeMap<String, MasterPosition>,
    key: &str,
    mv: &str,
    limit: usize,
) -> Result<bool> {
    let mut pos = position_from_sfen(&format!("{key} 1"))?;
    if apply_usi(&mut pos, mv).is_err() {
        return Ok(false);
    }
    let mut visited = BTreeSet::from([key.to_owned()]);
    for _ in 0..limit {
        let mut sfen = pos.to_sfen();
        if !positions.contains_key(strip_ply(&sfen)) {
            let Some(flipped) = rshogi_book::flipped_key(&sfen)
                .filter(|flipped| positions.contains_key(strip_ply(flipped)))
            else {
                return Ok(false);
            };
            pos = position_from_sfen(&flipped)?;
            sfen = flipped;
        }
        let key = strip_ply(&sfen);
        if !visited.insert(key.to_owned()) {
            return Ok(false);
        }
        let Some(main) = positions.get(key).and_then(|p| p.moves.first()) else {
            return Ok(false);
        };
        if apply_usi(&mut pos, &main.mv).is_err() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `--bridge-table` の 1 行。値は手番の側から。
#[derive(Debug, Clone, PartialEq)]
struct BridgeFinding {
    key: String,
    ply: String,
    main: String,
    /// 補わないときの main の値。
    plain: i32,
    /// 補ったときの main の値。
    bridged: i32,
    /// 補ったときの 1 位の手と値。
    best: String,
    best_value: i32,
    weight: Option<f64>,
}

/// 補ったとき (`book`) の main の値が、補わないときより `line` 以上悪くなった局面。
fn bridge_findings(
    positions: &BTreeMap<String, MasterPosition>,
    draws: DrawValues,
    book: &AuditedBook,
    line: i32,
    records: Option<&BTreeMap<String, MasterPosition>>,
) -> Result<Vec<BridgeFinding>> {
    // 補った伝播と同じく反転の辺を張らずに比べ、補ったことによる差だけを出す。
    let (plain, white_graph, _) = propagate_for_sides(positions, records, draws, Fill::Off, true)?;
    let plain = AuditedBook {
        graph: plain.graph,
        known: plain.known,
        white_graph,
        capped: plain.capped,
    };
    let mut out = Vec::new();
    for (((key, position), known), plain_known) in
        positions.iter().zip(&book.known).zip(&plain.known)
    {
        let Some(old) = audited_moves(plain.graph_for(key), key, position, plain_known)?
            .first()
            .cloned()
            .flatten()
            .map(|m| m.value)
        else {
            continue;
        };
        let moves = audited_moves(book.graph_for(key), key, position, known)?;
        let Some(main) = moves.first().cloned().flatten() else {
            continue;
        };
        if old - main.value < line {
            continue;
        }
        let best = moves
            .iter()
            .flatten()
            .max_by_key(|m| m.value)
            .context("内部エラー: 値の分かる手がありません")?;
        out.push(BridgeFinding {
            key: key.clone(),
            ply: position.ply.clone(),
            main: main.mv.clone(),
            plain: old,
            bridged: main.value,
            best: best.mv.clone(),
            best_value: best.value,
            weight: None,
        });
    }
    Ok(out)
}

fn weight_bridge_findings(rows: &mut Vec<BridgeFinding>, weights: Option<&SideWeights>, min: f64) {
    for row in rows.iter_mut() {
        let side = usize::from(super::model::side(&row.key) == "w");
        row.weight = weights.map(|w| w[side].get(&row.key).copied().unwrap_or(0.0));
    }
    rows.retain(|r| r.weight.is_none_or(|w| w >= min));
    rows.sort_by(|a, b| {
        b.weight
            .unwrap_or(0.0)
            .total_cmp(&a.weight.unwrap_or(0.0))
            .then_with(|| (b.plain - b.bridged).cmp(&(a.plain - a.bridged)))
            .then_with(|| a.key.cmp(&b.key))
    });
}

fn bridge_queue_line(row: &BridgeFinding, rank: usize, args: &AuditArgs) -> Result<Line> {
    let ply: u32 = row.ply.trim().parse().ok().filter(|&p| p > 0).with_context(|| {
        format!("キューの行には 1 以上の手数が要ります: {} {:?}", row.key, row.ply)
    })?;
    let side = super::model::side(&row.key);
    let digest = format!("{:x}", Sha256::digest(format!("{} {}", row.key, row.main)));
    let line = Line {
        group: None,
        id: format!("{}_bridge_{side}", &digest[..16]),
        root: format!("sfen {} {ply}", row.key),
        roots: Vec::new(),
        own: side.to_owned(),
        depth: args.bridge_depth,
        priority: args.bridge_priority + rank.min(99) as u32,
        hold: false,
        game: vec![vec![row.main.clone()]],
        opts: args.bridge_opts(),
        memo: Some(format!(
            "bridge main={} plain={} bridged={} best={} best_value={}",
            row.main, row.plain, row.bridged, row.best, row.best_value
        )),
    };
    line.check().with_context(|| format!("キューの行の検査: {}", row.key))?;
    Ok(line)
}

fn bridge_queue(rows: &[BridgeFinding], args: &AuditArgs) -> Result<(Vec<Line>, usize)> {
    let mut latest = BTreeMap::new();
    for path in &args.bridge_exclude_queue {
        read_checked_lines(path, |line, _, _| {
            latest.insert(line.id.clone(), line);
            Ok(())
        })?;
    }
    let mut covered = BTreeSet::new();
    for line in latest.into_values() {
        if line.hold || line.depth < args.bridge_exclude_min_depth {
            continue;
        }
        for root in std::iter::once(&line.root).chain(&line.roots) {
            let key = parse_root(root)?.key;
            if line.own.contains(super::model::side(&key)) {
                covered.insert(key);
            }
        }
    }
    let excluded = rows.iter().filter(|r| covered.contains(&r.key)).count();
    let lines = rows
        .iter()
        .filter(|r| !covered.contains(&r.key))
        .take(args.bridge_max.unwrap_or(usize::MAX))
        .enumerate()
        .map(|(rank, row)| bridge_queue_line(row, rank, args))
        .collect::<Result<Vec<_>>>()?;
    Ok((lines, excluded))
}

/// `--bridge-table` の TSV。値は手番の側からと、先手から見た値の両方。
fn format_bridge_table(rows: &[BridgeFinding]) -> String {
    let mut text = String::from(
        "key\tply\tside\tmain\tplain\tbridged\tdrop\tbest\tbest_value\tplain_black\tbridged_black\tweight\n",
    );
    for r in rows {
        let white = r.key.split(' ').nth(1) == Some("w");
        let black = |v: i32| if white { -v } else { v };
        let _ = writeln!(
            text,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.key,
            r.ply,
            if white { "w" } else { "b" },
            r.main,
            r.plain,
            r.bridged,
            r.plain - r.bridged,
            r.best,
            r.best_value,
            black(r.plain),
            black(r.bridged),
            r.weight.map_or_else(String::new, |w| format!("{w:.6}"))
        );
    }
    text
}

/// 元データを伝播して main を点検する。点検の表 (差の大きい順)・要約・伝播後のグラフを返す。
pub(super) fn audit(
    book: &(BTreeMap<String, MasterPosition>, usize),
    draws: DrawValues,
    line: i32,
    fill: Fill,
    deep_plies: Option<usize>,
) -> Result<(Vec<Finding>, AuditSummary, AuditedBook)> {
    audit_with_deep(book, draws, line, fill, deep_plies, None)
}

fn audit_with_deep(
    book: &(BTreeMap<String, MasterPosition>, usize),
    draws: DrawValues,
    line: i32,
    fill: Fill,
    deep_plies: Option<usize>,
    deep_freq: Option<&ReplyFrequency>,
) -> Result<(Vec<Finding>, AuditSummary, AuditedBook)> {
    audit_with_records(book, draws, line, fill, deep_plies, deep_freq, None)
}

fn audit_with_records(
    book: &(BTreeMap<String, MasterPosition>, usize),
    draws: DrawValues,
    line: i32,
    fill: Fill,
    deep_plies: Option<usize>,
    deep_freq: Option<&ReplyFrequency>,
    opponent_records: Option<&BTreeMap<String, MasterPosition>>,
) -> Result<(Vec<Finding>, AuditSummary, AuditedBook)> {
    let select = Select {
        line,
        rule: SwitchRule::Line,
        deep_plies,
        deep_freq,
    };
    audit_select(book, draws, select, fill, opponent_records)
}

/// 点検の表に出す局面の決め方。
#[derive(Clone, Copy)]
struct Select<'a> {
    line: i32,
    rule: SwitchRule,
    deep_plies: Option<usize>,
    deep_freq: Option<&'a ReplyFrequency>,
}

fn audit_select(
    (positions, records): &(BTreeMap<String, MasterPosition>, usize),
    draws: DrawValues,
    Select {
        line,
        rule,
        deep_plies,
        deep_freq,
    }: Select,
    fill: Fill,
    opponent_records: Option<&BTreeMap<String, MasterPosition>>,
) -> Result<(Vec<Finding>, AuditSummary, AuditedBook)> {
    let mut summary = AuditSummary {
        records: *records,
        ..AuditSummary::default()
    };
    let (propagated, white_graph, record_usage) =
        propagate_for_sides(positions, opponent_records, draws, fill, fill == Fill::Bridge)?;
    summary.opponent_record_positions = opponent_records.map(|_| record_usage.positions);
    summary.opponent_record_moves = record_usage.moves;
    summary.records -= record_usage.moves;
    let Propagated {
        graph,
        known,
        illegal_moves,
        stats,
        reply_positions,
        bridged_positions,
        capped,
    } = propagated;
    let book = AuditedBook {
        graph,
        known,
        white_graph,
        capped,
    };
    summary.illegal_moves = illegal_moves;
    summary.reply_positions = reply_positions;
    summary.bridged_positions = bridged_positions;
    summary.nontrivial_sccs = stats.nontrivial_sccs;
    summary.max_scc_size = stats.max_scc_size;
    let mut findings = Vec::new();
    for ((key, position), known) in positions.iter().zip(&book.known) {
        let side = usize::from(key.split(' ').nth(1) == Some("w"));
        summary.positions[side] += 1;
        summary.moves += position.moves.len();
        if position.moves.len() >= 2 {
            summary.multi_move_positions[side] += 1;
        }
        let audited = audited_moves(book.graph_for(key), key, position, known)?;
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
        // 伝播の書き出しは勝ち側の同値の手のうち出口まで短い手だけを残し、ほかを -1 する。補正後の値で
        // 比べると同値の局面で main が 1 位でなくなり、木ごとに main が行き来するので、補正を戻して比べる。
        // 同じ値なら main を 1 位とみなし、main 以外の同値は伝播が残した手を取る。
        let raw = |m: &AuditedMove| m.value + i32::from(m.tie_lowered);
        let (_, best) = audited
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.as_ref().map(|m| (i, m)))
            .reduce(|(bi, best), (i, m)| {
                let (v, bv) = (raw(m), raw(best));
                if v > bv || (v == bv && bi != 0 && best.tie_lowered && !m.tie_lowered) {
                    (i, m)
                } else {
                    (bi, best)
                }
            })
            .expect("main がある");
        let restore = |m: &AuditedMove| AuditedMove {
            value: raw(m),
            tie_lowered: false,
            ..m.clone()
        };
        let (mut main, mut best) = (restore(main), restore(best));
        let gap = best.value - main.value;
        if gap == 0 {
            continue;
        }
        if rule == SwitchRule::Propagated {
            // 補った相手の手番の局面で着いた手の読みの値が最善なら、辺があっても値は 1 回の読みなので数え直す。
            let graph = book.graph_for(key);
            let capped = |k: &str| book.capped.contains(k);
            let node =
                graph.find(key).context("内部エラー: main のある局面がグラフにありません")?;
            let in_graph: Vec<MoveValue> = graph.moves(node).collect();
            for m in [&mut main, &mut best] {
                let j = position
                    .moves
                    .iter()
                    .position(|x| x.mv == m.mv)
                    .context("内部エラー: 手がありません")?;
                let g = known[..j].iter().filter(|&&k| k).count();
                if !from_child(graph, capped, &in_graph[g]) {
                    m.origin = Origin::Eval;
                }
            }
            if gap < line {
                summary.below_line[side] += 1;
            } else {
                summary.at_or_above_line[side] += 1;
            }
        } else if gap < line {
            summary.below_line[side] += 1;
            let Some(limit) = deep_plies else {
                continue;
            };
            let continues = |mv: &str| -> Result<bool> {
                if let Some(freq) = deep_freq {
                    let continuation = Continuation {
                        freq,
                        main: |key: &str| {
                            if let Some(p) = positions.get(key) {
                                return p.moves.first().map(|m| m.mv.clone());
                            }
                            let flipped = rshogi_book::flipped_key(key)?;
                            let m = positions.get(strip_ply(&flipped))?.moves.first()?;
                            rshogi_book::flip_usi_move(&m.mv)
                        },
                    };
                    Ok(continuation.after(key, mv, limit)? >= limit)
                } else {
                    main_continues_after(positions, key, mv, limit)
                }
            };
            if !continues(&main.mv)? || !continues(&best.mv)? {
                continue;
            }
            summary.deep_below_line[side] += 1;
        } else {
            summary.at_or_above_line[side] += 1;
        }
        findings.push(Finding {
            key: key.clone(),
            ply: position.ply.clone(),
            source: position.source.clone(),
            main,
            best,
        });
    }
    findings.sort_by(|a, b| b.gap().cmp(&a.gap()).then_with(|| a.key.cmp(&b.key)));
    Ok((findings, summary, book))
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

pub(super) fn format_summary(
    summary: &AuditSummary,
    line: i32,
    draws: DrawValues,
    deep_plies: Option<usize>,
) -> String {
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
        "手 (main・alt): {} / 値の分かる手 {} / 非合法手 {} / bench・record (除外) {}",
        summary.moves, summary.known_moves, summary.illegal_moves, summary.records
    ));
    if let Some(count) = summary.opponent_record_positions {
        out(format!(
            "相手の手として使った record {} / 局面 {count}",
            summary.opponent_record_moves
        ));
    }
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
    if let Some(limit) = deep_plies {
        out(format!(
            "両方の先が {limit} 手以上続くので線未満でも対象にした局面: 先手番 {}・後手番 {}",
            summary.deep_below_line[0], summary.deep_below_line[1]
        ));
    }
    out(format!(
        "main の値が分からない局面: {} / うち main・alt が 2 手以上: {}",
        sides(summary.unknown_main),
        sides(summary.unknown_main_multi)
    ));
    out(format!(
        "補った相手の手番の局面 (元データに無く、応手が元データの局面に着く): {} / うち読みの値を残した局面: {}",
        summary.reply_positions, summary.bridged_positions
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
        let (findings, summary, _) = audit(&parsed, draws, line, Fill::Off, None)?;
        Ok((findings, summary))
    }

    fn opponent_record_fixture(record_eval: &str) -> String {
        master(&[
            (A, "5a5b", "main", "200"),
            (A, "5a4a", "alt", "-100"),
            ("5k3/9/9/9/9/9/9/5K3/9 b -", "4h3h", "main", "100"),
            (B, "4h5h", "main", "-200"),
            (B, "4h3g", "record", record_eval),
            ("9/4k4/9/9/9/9/6K2/9/9 w -", "5b6b", "main", "-900"),
        ])
    }

    #[test]
    fn opponent_records_replace_white_main_and_preserve_record_role() {
        for full in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut args = test_args(dir.path());
            let text = opponent_record_fixture("400");
            std::fs::write(&args.master, &text).unwrap();
            args.opp_records = true;
            args.apply = full;
            args.since = Some("確認".into());
            args.out_db = Some(dir.path().join("audit.db"));
            let master_path = args.master.clone();
            let table_path = args.table.clone();
            let db_path = args.out_db.clone().unwrap();
            run(args).unwrap();
            let table = std::fs::read_to_string(table_path).unwrap();
            assert!(table.contains("5a4a"));
            assert!(table.contains("-900"));
            let db = std::fs::read_to_string(db_path).unwrap();
            assert!(db.contains("5a5b none -900"));
            assert!(!db.contains("4h3g"));
            let rows = parse_master(&std::fs::read_to_string(master_path).unwrap()).unwrap();
            assert_eq!(
                rows.iter().find(|r| r.get("role") == "record").unwrap().get("move"),
                "4h3g"
            );
            assert_eq!(
                rows.iter()
                    .find(|r| r.get("key") == A && r.get("role") == "main")
                    .unwrap()
                    .get("move"),
                if full { "5a4a" } else { "5a5b" }
            );
        }
    }

    #[test]
    fn opponent_records_do_not_use_own_records_or_unread_records() {
        for eval in ["400", ""] {
            let text = opponent_record_fixture(eval);
            let rows = parse_master(&text).unwrap();
            let positions = parse_master_rows(text.as_bytes()).unwrap();
            let records = record_positions(&rows).unwrap();
            let (findings, summary, book) = audit_with_records(
                &positions,
                DrawValues::both(0),
                1,
                Fill::Off,
                None,
                None,
                Some(&records),
            )
            .unwrap();
            let index = positions.0.keys().position(|k| k == B).unwrap();
            let moves =
                audited_moves(book.graph_for(B), B, &positions.0[B], &book.known[index]).unwrap();
            assert_eq!(moves.len(), 1);
            assert_eq!(moves[0].as_ref().unwrap().value, -200);
            assert_eq!(summary.opponent_record_positions, Some(usize::from(!eval.is_empty())));
            assert_eq!(findings.iter().any(|f| f.key == A), !eval.is_empty());
        }
    }

    #[test]
    fn opponent_records_demotion_and_revival_use_white_values() {
        for demote in [false, true] {
            for enabled in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let text = master(&[
                    (A, "5a4a", "main", "-100"),
                    (A, "5a5b", if demote { "alt" } else { "bench" }, "200"),
                    (B, "4h5h", "main", "-200"),
                    (B, "4h3g", "record", "900"),
                ]);
                let mut args = test_args(dir.path());
                std::fs::write(&args.master, text).unwrap();
                args.opp_records = enabled;
                args.demote_alts = demote;
                args.revive_bench = !demote;
                args.apply = true;
                args.line = 10000;
                args.since = Some("確認".into());
                let path = args.master.clone();
                run(args).unwrap();
                let rows = parse_master(&std::fs::read_to_string(path).unwrap()).unwrap();
                let candidate =
                    rows.iter().find(|r| r.get("key") == A && r.get("move") == "5a5b").unwrap();
                assert_eq!(candidate.get("role"), if enabled { "bench" } else { "alt" });
            }
        }
    }

    #[test]
    fn opponent_records_include_record_only_positions_and_both_sides() {
        let text = master(&[
            (ROOT, "5i4h", "main", "200"),
            (ROOT, "5i6h", "alt", "-100"),
            (A, "5a5b", "record", "900"),
            (A, "5a4a", "record", "800"),
            (B, "4h3g", "main", "-400"),
        ]);
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let records = record_positions(&parse_master(&text).unwrap()).unwrap();
        let (findings, summary, book) = audit_with_records(
            &positions,
            DrawValues::both(0),
            1,
            Fill::Off,
            None,
            None,
            Some(&records),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.key == ROOT && f.best.mv == "5i6h"));
        assert_eq!(summary.opponent_record_positions, Some(1));
        assert_eq!(book.known.len(), positions.0.len());
        assert!(!positions.0.contains_key(A));
    }

    #[test]
    fn opponent_records_skip_flipped_children_for_both_sides() {
        for flipped in [false, true] {
            for direct_record in [false, true] {
                let mut input = vec![
                    (ROOT, "5i4h", "main", "200"),
                    (A_FLIPPED, "5i5h", "main", "30"),
                    (A_FLIPPED, "5i6h", "record", "900"),
                    ("5k3/9/9/9/9/9/9/9/4K4 w -", "4a5a", "main", "100"),
                ];
                if direct_record {
                    input.push((A, "5a5b", "record", "800"));
                }
                let mut rows = parse_master(&master(&input)).unwrap();
                if flipped {
                    for row in &mut rows {
                        let key = rshogi_book::flipped_key(row.get("key")).unwrap();
                        row.set("key", strip_ply(&key).to_owned());
                        row.set("move", rshogi_book::flip_usi_move(row.get("move")).unwrap());
                    }
                }
                let key = rows[0].get("key");
                let positions = parse_master_rows(format_master(&rows).as_bytes()).unwrap();
                let records = record_positions(&rows).unwrap();
                let (_, _, book) = audit_with_records(
                    &positions,
                    DrawValues::both(0),
                    1,
                    Fill::Off,
                    None,
                    None,
                    Some(&records),
                )
                .unwrap();
                let graph = book.graph_for(key);
                assert!(graph.options.skip_flipped_children);
                let value = graph.moves(graph.find(key).unwrap()).next().unwrap();
                assert_eq!(value.new, if direct_record { -800 } else { 200 });
                assert_eq!(value.edge.is_some(), direct_record);
                let parent = rows[3].get("key");
                let graph = book.graph_for(parent);
                let value = graph.moves(graph.find(parent).unwrap()).next().unwrap();
                assert_eq!(value.new, -200);
                assert!(value.edge.is_some());
            }
        }
    }

    #[test]
    fn opponent_records_summary_separates_used_records_from_exclusions_and_illegal_moves() {
        let text = master(&[
            (ROOT, "1a1b", "main", "0"),
            (ROOT, "5i6h", "alt", "0"),
            (ROOT, "5i5h", "bench", "0"),
            (ROOT, "5i4h", "record", "10"),
            (ROOT, "5i6i", "record", "40"),
            (ROOT, "5i4i", "record", ""),
            (ROOT, "1b1c", "record", "20"),
            (A, "5a5b", "record", "900"),
            (A, "5a4a", "record", "800"),
            (A, "5a6a", "record", ""),
            (A, "1a1b", "record", "30"),
        ]);
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let records = record_positions(&parse_master(&text).unwrap()).unwrap();
        let (_, summary, _) = audit_with_records(
            &positions,
            DrawValues::both(0),
            1,
            Fill::Off,
            None,
            None,
            Some(&records),
        )
        .unwrap();
        assert_eq!(summary.moves, 2);
        assert_eq!(summary.illegal_moves, 1);
        assert_eq!(summary.records, 5);
        assert_eq!(summary.opponent_record_moves, 4);
        assert_eq!(summary.opponent_record_positions, Some(2));
        let text = format_summary(&summary, 1, DrawValues::both(0), None);
        assert!(text.contains("非合法手 1 / bench・record (除外) 5"));
        assert!(text.contains("相手の手として使った record 4 / 局面 2"));
    }

    #[test]
    fn audit_without_opponent_records_preserves_outputs() {
        const EXPECTED_SUMMARY: &str = "千日手の値: 先手 0 / 後手 0
局面: 4 (先手番 2 / 後手番 2)
手 (main・alt): 5 / 値の分かる手 5 / 非合法手 0 / bench・record (除外) 1
読みの値から伝播で変わった手: 0
読みの値が無く伝播で値が付いた手: 0
main・alt が 2 手以上の局面: 1 (先手番 0 / 後手番 1)
main が 1 位から 1〜0 負ける局面: 0 (先手番 0 / 後手番 0)
main が 1 位から 1 以上負ける局面: 0 (先手番 0 / 後手番 0)
main の値が分からない局面: 0 (先手番 0 / 後手番 0) / うち main・alt が 2 手以上: 0 (先手番 0 / 後手番 0)
補った相手の手番の局面 (元データに無く、応手が元データの局面に着く): 0 / うち読みの値を残した局面: 0
循環 (自明でない強連結成分): 0 (最大 1 局面)
";
        const EXPECTED_TABLE: &str = "key\tply\tside\tsource\tmain\tmain_value\tmain_from\tmain_eval\tbest\tbest_value\tbest_from\tbest_eval\tgap\tmain_value_black\tbest_value_black\n";
        const EXPECTED_DB: &str = "#YANEURAOU-DB2016 1.00
sfen 4k4/9/9/9/9/9/9/5K3/9 w - 1
5a5b none 200 1 2
5a4a none -100 1 1
sfen 5k3/9/9/9/9/9/9/5K3/9 b - 1
4h3h none 100 1 2
sfen 9/4k4/9/9/9/9/6K2/9/9 w - 1
5b6b none -900 1 2
sfen 9/4k4/9/9/9/9/9/5K3/9 b - 1
4h5h none -200 1 2
";
        let text = opponent_record_fixture("400");
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let (findings, summary, _) =
            audit(&positions, DrawValues::both(0), 1, Fill::Off, None).unwrap();
        assert!(findings.is_empty());
        assert_eq!(format_summary(&summary, 1, DrawValues::both(0), None), EXPECTED_SUMMARY);
        for full in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut args = test_args(dir.path());
            std::fs::write(&args.master, &text).unwrap();
            args.apply = full;
            args.since = Some("確認".into());
            args.out_db = Some(dir.path().join("actual.db"));
            let table = args.table.clone();
            let db = args.out_db.clone().unwrap();
            let master = args.master.clone();
            run(args).unwrap();
            assert_eq!(std::fs::read_to_string(table).unwrap(), EXPECTED_TABLE);
            assert_eq!(std::fs::read_to_string(db).unwrap(), EXPECTED_DB);
            assert_eq!(std::fs::read_to_string(master).unwrap(), text);
        }
    }

    #[test]
    fn opponent_records_bridge_replies_and_bridge_table_use_same_side() {
        let text = master(&[
            (A, "5a5b", "main", "200"),
            (A, "5a4a", "alt", "-100"),
            ("5k3/9/9/9/9/9/9/5K3/9 b -", "4h3h", "main", "100"),
            (B, "4h5h", "main", "-200"),
            (B, "4h3g", "record", "900"),
            ("9/3k5/9/9/9/9/6K2/9/9 b -", "3g2f", "main", "400"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args(dir.path());
        std::fs::write(&args.master, text).unwrap();
        args.opp_records = true;
        args.bridge_replies = true;
        args.apply = true;
        args.since = Some("確認".into());
        args.bridge_table = Some(dir.path().join("bridge.tsv"));
        let bridge_table = args.bridge_table.clone().unwrap();
        let master_path = args.master.clone();
        run(args).unwrap();
        let table = std::fs::read_to_string(bridge_table).unwrap();
        assert!(!table.contains(A));
        assert!(std::fs::read_to_string(dir.path().join("audit.tsv")).unwrap().contains("-400"));
        let rows = parse_master(&std::fs::read_to_string(master_path).unwrap()).unwrap();
        assert_eq!(
            rows.iter()
                .find(|r| r.get("key") == A && r.get("role") == "main")
                .unwrap()
                .get("move"),
            "5a4a"
        );
        assert_eq!(rows.iter().find(|r| r.get("move") == "4h3g").unwrap().get("role"), "record");
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
    fn tied_main_stays_first_even_when_lowered_by_tie_break() {
        // 5i4h (A → B の 2 手先の読み) と 5i6h (1 手先の読み) が同値 10 で、出口まで短い 5i6h が残り
        // main の 5i4h が -1 される。
        let tied = |alt_reply: &str| {
            master(&[
                (ROOT, "5i4h", "main", ""),
                (ROOT, "5i6h", "alt", ""),
                (A, "5a5b", "main", ""),
                (B, "4h4g", "main", "10"),
                ("4k4/9/9/9/9/9/9/3K5/9 w -", "5a5b", "main", alt_reply),
            ])
        };
        let text = tied("-10");
        let parsed = parse_master_rows(text.as_bytes()).unwrap();
        let (findings, _, book) = audit(&parsed, DrawValues::both(0), 1, Fill::Off, None).unwrap();
        let index = parsed.0.keys().position(|k| k == ROOT).unwrap();
        let root =
            audited_moves(book.graph_for(ROOT), ROOT, &parsed.0[ROOT], &book.known[index]).unwrap();
        assert!(root[0].as_ref().is_some_and(|m| m.tie_lowered && m.value == 9));
        assert!(findings.is_empty());

        let (findings, _) = run_audit(&tied("-11"), 1);
        assert_eq!(findings.len(), 1);
        assert_eq!((findings[0].main.value, findings[0].best.mv.as_str()), (10, "5i6h"));
        assert_eq!(findings[0].gap(), 1);
    }

    fn deep_fixture() -> String {
        master(&[
            (ROOT, "5i4h", "main", ""),
            (ROOT, "5i6h", "alt", ""),
            (A, "5a5b", "main", "-20"),
            (B, "4h4g", "main", "20"),
            ("4k4/9/9/9/9/9/9/3K5/9 w -", "5a5b", "main", "-40"),
            ("9/4k4/9/9/9/9/9/3K5/9 b -", "6h6g", "main", "40"),
        ])
    }

    #[test]
    fn deep_frequency_requires_coverage_of_frequent_replies() {
        for flipped in [false, true] {
            for (count, minimum, enabled, expected) in [
                (3, 3, true, 0),
                (2, 3, true, 1),
                (44, 45, true, 1),
                (44, 3, false, 1),
            ] {
                let parsed = parse_master_rows(deep_fixture().as_bytes()).unwrap();
                let (key, mv) = if flipped {
                    (rshogi_book::flipped_key(A).unwrap(), "5i6h")
                } else {
                    (format!("{A} 1"), "5a4b")
                };
                let freq = ReplyFrequency::from_reader(
                    format!("sfen {key}\n{mv} none 0 0 {count}\n").as_bytes(),
                    minimum,
                )
                .unwrap();
                let (findings, summary, _) = audit_with_deep(
                    &parsed,
                    DrawValues::both(0),
                    40,
                    Fill::Off,
                    Some(2),
                    enabled.then_some(&freq),
                )
                .unwrap();
                assert_eq!(findings.len(), expected);
                assert_eq!(summary.deep_below_line, [expected, 0]);
            }
        }
    }

    #[test]
    fn deep_plies_includes_below_line_when_both_continuations_are_long_enough() {
        let parsed = parse_master_rows(deep_fixture().as_bytes()).unwrap();
        let (findings, summary, _) =
            audit(&parsed, DrawValues::both(0), 40, Fill::Off, Some(2)).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].key, ROOT);
        assert_eq!(findings[0].gap(), 20);
        assert_eq!(summary.below_line, [1, 0]);
        assert_eq!(summary.at_or_above_line, [0, 0]);
        assert_eq!(summary.deep_below_line, [1, 0]);
        assert!(
            format_summary(&summary, 40, DrawValues::both(0), Some(2)).contains(
                "両方の先が 2 手以上続くので線未満でも対象にした局面: 先手番 1・後手番 0"
            )
        );
    }

    #[test]
    fn deep_plies_follows_flipped_continuation() {
        let mut rows = parse_master(&deep_fixture()).unwrap();
        for row in &mut rows {
            if [A, B].contains(&row.get("key")) {
                let flipped = rshogi_book::flipped_key(row.get("key")).unwrap();
                row.set("key", strip_ply(&flipped).to_owned());
                row.set("move", rshogi_book::flip_usi_move(row.get("move")).unwrap());
            }
        }
        let parsed = parse_master_rows(format_master(&rows).as_bytes()).unwrap();
        let (findings, summary, _) =
            audit(&parsed, DrawValues::both(0), 40, Fill::Off, Some(2)).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].key, ROOT);
        assert_eq!(findings[0].gap(), 20);
        assert_eq!(summary.deep_below_line, [1, 0]);
        let empty = ReplyFrequency::default();
        let (findings, _, _) =
            audit_with_deep(&parsed, DrawValues::both(0), 40, Fill::Off, Some(2), Some(&empty))
                .unwrap();
        assert_eq!(findings.len(), 1);
        let freq =
            ReplyFrequency::from_reader(format!("sfen {A} 1\n5a4b none 0 0 44\n").as_bytes(), 3)
                .unwrap();
        let (findings, _, _) =
            audit_with_deep(&parsed, DrawValues::both(0), 40, Fill::Off, Some(2), Some(&freq))
                .unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn deep_plies_includes_below_line_for_white() {
        let mut rows = parse_master(&deep_fixture()).unwrap();
        for row in &mut rows {
            let flipped = rshogi_book::flipped_key(row.get("key")).unwrap();
            row.set("key", strip_ply(&flipped).to_owned());
            row.set("move", rshogi_book::flip_usi_move(row.get("move")).unwrap());
        }
        let parsed = parse_master_rows(format_master(&rows).as_bytes()).unwrap();
        let (findings, summary, _) =
            audit(&parsed, DrawValues::both(0), 40, Fill::Off, Some(2)).unwrap();
        assert_eq!(findings.len(), 1);
        let flipped_root = rshogi_book::flipped_key(ROOT).unwrap();
        assert_eq!(findings[0].key, strip_ply(&flipped_root));
        assert_eq!(findings[0].gap(), 20);
        assert_eq!(summary.below_line, [0, 1]);
        assert_eq!(summary.at_or_above_line, [0, 0]);
        assert_eq!(summary.deep_below_line, [0, 1]);
    }

    #[test]
    fn deep_plies_keeps_at_or_above_line_with_short_continuations() {
        let text = master(&[(ROOT, "5i4h", "main", "0"), (ROOT, "5i6h", "alt", "40")]);
        let parsed = parse_master_rows(text.as_bytes()).unwrap();
        let (findings, summary, _) =
            audit(&parsed, DrawValues::both(0), 40, Fill::Off, Some(2)).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].key, ROOT);
        assert_eq!(findings[0].gap(), 40);
        assert_eq!(summary.at_or_above_line, [1, 0]);
        assert_eq!(summary.below_line, [0, 0]);
        assert_eq!(summary.deep_below_line, [0, 0]);
    }

    #[test]
    fn deep_plies_excludes_below_line_when_either_continuation_is_short() {
        for short_leaf in [B, "9/4k4/9/9/9/9/9/3K5/9 b -"] {
            let mut rows = parse_master(&deep_fixture()).unwrap();
            rows.retain(|row| row.get("key") != short_leaf);
            let parsed = parse_master_rows(format_master(&rows).as_bytes()).unwrap();
            let (findings, summary, _) =
                audit(&parsed, DrawValues::both(0), 40, Fill::Off, Some(2)).unwrap();
            assert!(findings.is_empty(), "{short_leaf}");
            assert_eq!(summary.below_line, [1, 0]);
            assert_eq!(summary.deep_below_line, [0, 0]);
        }
        let parsed = parse_master_rows(deep_fixture().as_bytes()).unwrap();
        let (findings, _, _) = audit(&parsed, DrawValues::both(0), 40, Fill::Off, Some(3)).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn without_deep_plies_long_continuations_still_respect_line() {
        let (findings, summary) = run_audit(&deep_fixture(), 40);
        assert!(findings.is_empty());
        assert_eq!(summary.below_line, [1, 0]);
        assert_eq!(summary.deep_below_line, [0, 0]);
        assert!(!format_summary(&summary, 40, DrawValues::both(0), None).contains("両方の先が"));
    }

    #[test]
    fn main_continuation_stops_at_repeated_position() {
        let text = master(&[
            (ROOT, "5i4h", "main", "0"),
            (A, "5a5b", "main", "0"),
            (B, "4h5i", "main", "0"),
            ("9/4k4/9/9/9/9/9/9/4K4 w -", "5b5a", "main", "0"),
        ]);
        let (positions, _) = parse_master_rows(text.as_bytes()).unwrap();
        assert!(main_continues_after(&positions, ROOT, "5i4h", 3).unwrap());
        assert!(!main_continues_after(&positions, ROOT, "5i4h", 4).unwrap());
        assert!(!main_continues_after(&positions, ROOT, "5i4h", usize::MAX).unwrap());
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
            let (memory, actual_known, illegal, _) =
                propagate_entries(&entries, draws, false).unwrap();
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
                    white_graph: None,
                    capped: BTreeSet::new(),
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
        let (_, summary, book) =
            audit(&parsed, DrawValues::both(0), 1, Fill::Unread, None).unwrap();
        let index = parsed.0.keys().position(|k| k == ROOT).unwrap();
        let main = audited_moves(&book.graph, ROOT, &parsed.0[ROOT], &book.known[index]).unwrap()
            [0]
        .as_ref()
        .map(|m| m.value);
        (summary, main)
    }

    /// `--bridge-replies` で点検し、(要約, ROOT の main の伝播後の値) を返す。
    fn bridge_audit(rows: &[(&str, &str, &str, &str)]) -> (AuditSummary, Option<i32>) {
        let parsed = parse_master_rows(master(rows).as_bytes()).unwrap();
        let (_, summary, book) =
            audit(&parsed, DrawValues::both(0), 1, Fill::Bridge, None).unwrap();
        let index = parsed.0.keys().position(|k| k == ROOT).unwrap();
        let main = audited_moves(&book.graph, ROOT, &parsed.0[ROOT], &book.known[index]).unwrap()
            [0]
        .as_ref()
        .map(|m| m.value);
        (summary, main)
    }

    #[test]
    fn bridge_replies_takes_book_reply_when_worse_than_read() {
        // ROOT の 5i4h の読みは +50 だが、A の応手 5a5b で着く B の読みは -100 (先手から)。相手は悪い方を選ぶ。
        let (summary, main) =
            bridge_audit(&[(ROOT, "5i4h", "main", "50"), (B, "4h3h", "main", "-100")]);
        assert_eq!((summary.reply_positions, summary.bridged_positions, main), (1, 1, Some(-100)));
        // 補わなければ読みの値のまま。
        let (_, main) = fill_audit(&[(ROOT, "5i4h", "main", "50"), (B, "4h3h", "main", "-100")]);
        assert_eq!(main, Some(50));
    }

    #[test]
    fn opponent_records_keep_the_read_value_as_a_cap_on_record_only_positions() {
        // A は record だけの局面。record の 5a5b で着く B は先手 +200 だが、5i4h の読みの値 +50 が上限になる。
        let text = master(&[
            (ROOT, "5i4h", "main", "50"),
            (A, "5a5b", "record", "-200"),
            (B, "4h3h", "main", "200"),
        ]);
        let rows = parse_master(&text).unwrap();
        let parsed = parse_master_rows(text.as_bytes()).unwrap();
        let records = record_positions(&rows).unwrap();
        let main = |fill| {
            let (_, _, book) = audit_with_records(
                &parsed,
                DrawValues::both(0),
                1,
                fill,
                None,
                None,
                Some(&records),
            )
            .unwrap();
            let index = parsed.0.keys().position(|k| k == ROOT).unwrap();
            audited_moves(&book.graph, ROOT, &parsed.0[ROOT], &book.known[index]).unwrap()[0]
                .as_ref()
                .map(|m| m.value)
        };
        assert_eq!(main(Fill::Off), Some(50));
        assert_eq!(main(Fill::Bridge), Some(50));
        // record の先が読みの値より悪ければ、その値になる。
        let text = master(&[
            (ROOT, "5i4h", "main", "50"),
            (A, "5a5b", "record", "100"),
            (B, "4h3h", "main", "-100"),
        ]);
        let rows = parse_master(&text).unwrap();
        let parsed = parse_master_rows(text.as_bytes()).unwrap();
        let records = record_positions(&rows).unwrap();
        let (_, _, book) = audit_with_records(
            &parsed,
            DrawValues::both(0),
            1,
            Fill::Off,
            None,
            None,
            Some(&records),
        )
        .unwrap();
        let index = parsed.0.keys().position(|k| k == ROOT).unwrap();
        let value = audited_moves(&book.graph, ROOT, &parsed.0[ROOT], &book.known[index]).unwrap()
            [0]
        .as_ref()
        .map(|m| m.value);
        assert_eq!(value, Some(-100));
    }

    #[test]
    fn record_cap_avoids_destinations_that_bridge_replies_would_fill() {
        // 上限の手の最初の候補 X の行き先 D から、元データの局面 P に着く応手がある。D は --bridge-replies で
        // 補われて上限の値が置き換わるので、X は選ばない。
        let mut pos = position_from_sfen(&format!("{A} 1")).unwrap();
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        let x = legal.iter().map(|m| m.to_usi()).find(|m| m != "5a5b").unwrap();
        let d = crate::book_tree::model::child(A, &x).unwrap();
        pos = position_from_sfen(&format!("{d} 1")).unwrap();
        let mut replies = MoveList::new();
        generate_legal_all(&pos, &mut replies);
        let p =
            crate::book_tree::model::child(&d, &replies.iter().next().unwrap().to_usi()).unwrap();
        let text = master(&[
            (ROOT, "5i4h", "main", "50"),
            (A, "5a5b", "record", "-300"),
            (&p, "5a5b", "main", "-500"),
        ]);
        let parsed = parse_master_rows(text.as_bytes()).unwrap();
        let records = record_positions(&parse_master(&text).unwrap()).unwrap();
        let caps = record_caps(&parsed.0, &records, "w").unwrap();
        let cap = caps[A].as_ref().unwrap();
        assert_ne!(cap.mv, x);
        let (_, _, book) = audit_with_records(
            &parsed,
            DrawValues::both(0),
            1,
            Fill::Bridge,
            None,
            None,
            Some(&records),
        )
        .unwrap();
        let index = parsed.0.keys().position(|k| k == ROOT).unwrap();
        let value = audited_moves(&book.graph, ROOT, &parsed.0[ROOT], &book.known[index]).unwrap()
            [0]
        .as_ref()
        .map(|m| m.value);
        assert_eq!(value, Some(50));
    }

    #[test]
    fn record_only_positions_are_capped_even_when_no_reply_reaches_the_master() {
        // A の record はどれも元データの外へ行く (先の局面の値は record の evals)。record に無い合法手に
        // 読みの値 +50 を置くので、5i4h は +50 より良くならない。合法手を全部 record が持てば上限を置けないので、
        // record の局面を伝播に入れず、読みの値がそのまま効く。
        for recorded in [
            &["5a5b", "5a4a", "5a6a"][..],
            &["5a5b", "5a4a", "5a6a", "5a4b", "5a6b"],
        ] {
            let mut rows = vec![(ROOT, "5i4h", "main", "50")];
            for mv in recorded {
                rows.push((A, mv, "record", "-300"));
            }
            let text = master(&rows);
            let parsed = parse_master_rows(text.as_bytes()).unwrap();
            let records = record_positions(&parse_master(&text).unwrap()).unwrap();
            let caps = record_caps(&parsed.0, &records, "w").unwrap();
            match &caps[A] {
                Some(cap) => {
                    assert_eq!(recorded.len(), 3);
                    assert!(!records[A].moves.iter().any(|m| m.mv == cap.mv));
                    assert_eq!(cap.eval, Some(-50));
                }
                None => assert_eq!(recorded.len(), 5),
            }
            for fill in [Fill::Off, Fill::Bridge] {
                let (_, _, book) = audit_with_records(
                    &parsed,
                    DrawValues::both(0),
                    1,
                    fill,
                    None,
                    None,
                    Some(&records),
                )
                .unwrap();
                let index = parsed.0.keys().position(|k| k == ROOT).unwrap();
                let value = audited_moves(&book.graph, ROOT, &parsed.0[ROOT], &book.known[index])
                    .unwrap()[0]
                    .as_ref()
                    .map(|m| m.value);
                assert_eq!(value, Some(50), "{recorded:?} {fill:?}");
            }
        }
    }

    #[test]
    fn bridge_replies_keeps_read_when_book_reply_is_better() {
        // B が +200 でも、読みの値 +50 は定跡に無い応手も見た値なので、それより甘くしない。
        let (_, main) = bridge_audit(&[(ROOT, "5i4h", "main", "50"), (B, "4h3h", "main", "200")]);
        assert_eq!(main, Some(50));
    }

    #[test]
    fn bridge_replies_uses_worst_read_of_parents() {
        // ROOT (+50) と c (+10、どちらも先手から) の手が同じ A に着く。A に残すのは先手に悪い +10 で、ROOT の手にも効く。
        let c = "4k4/9/9/9/9/9/9/4K4/9 b -";
        let (_, main) = bridge_audit(&[
            (ROOT, "5i4h", "main", "50"),
            (c, "5h4h", "main", "10"),
            (B, "4h3h", "main", "200"),
        ]);
        assert_eq!(main, Some(10));
    }

    #[test]
    fn bridge_replies_keeps_cap_when_outside_move_reaches_another_filled_position() {
        // A に読みの値 +50 を残す手 5a4a の行き先は、c (後手番) の手の行き先として補う局面でもある。そこへ辺を
        // 張らせると残した値が補った局面の値で置き換わり、ROOT の手が +50 より甘くなる。
        let c = "6k2/9/9/9/9/9/9/5K3/9 w -";
        let d = "5k3/9/9/9/9/9/9/9/4K4 w -";
        let (_, main) = bridge_audit(&[
            (ROOT, "5i4h", "main", "50"),
            (B, "4h3h", "main", "200"),
            (c, "3a4a", "main", "-200"),
            (d, "4a3a", "main", "-200"),
        ]);
        assert!(main.is_some_and(|v| v <= 50), "{main:?}");
    }

    #[test]
    fn bridge_replies_flips_sign_for_white_mover() {
        // 後手の手 5a4b (読み +30、後手から) の後の先手番の局面 E は元データに無く、E の ▲5i4h で着く局面の読みが
        // 後手から -100。後手は悪い方を取られて -100 になる。
        let w = "4k4/9/9/9/9/9/9/9/4K4 w -";
        let f = "9/5k3/9/9/9/9/9/5K3/9 w -";
        let parsed = parse_master_rows(
            master(&[(w, "5a4b", "main", "30"), (f, "4b5b", "main", "-100")]).as_bytes(),
        )
        .unwrap();
        let (_, summary, book) =
            audit(&parsed, DrawValues::both(0), 1, Fill::Bridge, None).unwrap();
        let index = parsed.0.keys().position(|k| k == w).unwrap();
        let main = audited_moves(&book.graph, w, &parsed.0[w], &book.known[index]).unwrap()[0]
            .as_ref()
            .map(|m| m.value);
        assert_eq!((summary.bridged_positions, main), (1, Some(-100)));
    }

    #[test]
    fn bridge_replies_apply_replaces_main() {
        // 補うと main 5i4h は B の -100 になり、元データの局面 c に着く 5i6h (+20) が 1 位になるので、--apply で
        // main を替える。補った局面 (A) は元データに書かない。
        let c = "4k4/9/9/9/9/9/9/3K5/9 w -";
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args(dir.path());
        std::fs::write(
            &args.master,
            master(&[
                (ROOT, "5i4h", "main", "50"),
                (ROOT, "5i6h", "alt", "20"),
                (B, "4h3h", "main", "-100"),
                (c, "5a4a", "main", "-20"),
            ]),
        )
        .unwrap();
        args.bridge_replies = true;
        args.apply = true;
        args.since = Some("t2".into());
        args.line = 40;
        run_in_pool(args).unwrap();
        let text = std::fs::read_to_string(dir.path().join("moves.tsv")).unwrap();
        let rows = super::super::master_table::parse_master(&text).unwrap();
        let main: Vec<_> = rows
            .iter()
            .filter(|r| r.get("key") == ROOT && r.get("role") == "main")
            .map(|r| r.get("move").to_owned())
            .collect();
        assert_eq!(main, ["5i6h"]);
        assert!(!rows.iter().any(|r| r.get("key") == A));
    }

    /// `--switch-rule propagated` で `--apply` し、(ROOT の main, キューの行) を返す。
    fn propagated_switch(rows: &[(&str, &str, &str, &str)], bridge: bool) -> (String, Vec<Line>) {
        propagated_switch_with(rows, bridge, false)
    }

    fn propagated_switch_with(
        rows: &[(&str, &str, &str, &str)],
        bridge: bool,
        opp_records: bool,
    ) -> (String, Vec<Line>) {
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args(dir.path());
        std::fs::write(&args.master, master(rows)).unwrap();
        let queue = dir.path().join("single.jsonl");
        args.bridge_replies = bridge;
        args.opp_records = opp_records;
        args.apply = true;
        args.since = Some("t2".into());
        args.line = 40;
        args.switch_rule = SwitchRule::Propagated;
        args.single.single_enqueue_out = Some(queue.clone());
        run_in_pool(args).unwrap();
        let text = std::fs::read_to_string(dir.path().join("moves.tsv")).unwrap();
        let rows = super::super::master_table::parse_master(&text).unwrap();
        let main = rows
            .iter()
            .find(|r| r.get("key") == ROOT && r.get("role") == "main")
            .unwrap()
            .get("move")
            .to_owned();
        let lines = std::fs::read_to_string(queue)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (main, lines)
    }

    #[test]
    fn propagated_rule_switches_on_any_gap_when_both_values_come_from_children() {
        let c = "4k4/9/9/9/9/9/9/3K5/9 w -";
        let rows = [
            (ROOT, "5i4h", "main", "10"),
            (ROOT, "5i6h", "alt", "20"),
            (A, "5a4a", "main", "-15"),
            (c, "5a4a", "main", "-16"),
        ];
        let (main, lines) = propagated_switch(&rows, false);
        assert_eq!(main, "5i6h");
        assert!(lines.is_empty());
        // 同点なら main を残す。
        let mut tied = rows;
        tied[3].3 = "-15";
        assert_eq!(propagated_switch(&tied, false).0, "5i4h");
    }

    #[test]
    fn propagated_rule_keeps_main_and_queues_single_reads() {
        let c = "4k4/9/9/9/9/9/9/3K5/9 w -";
        // 1 位 5i5h は 1 回の読みの値。
        let (main, lines) = propagated_switch(
            &[
                (ROOT, "5i4h", "main", "10"),
                (ROOT, "5i5h", "alt", "99"),
                (A, "5a4a", "main", "-15"),
            ],
            false,
        );
        assert_eq!(main, "5i4h");
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line.root, format!("sfen {ROOT} 1"));
        assert_eq!((line.own.as_str(), line.depth), ("b", 6));
        assert_eq!(line.game, vec![vec!["5i5h".to_owned()]]);
        assert_eq!(
            (line.opts.opp_n, line.opts.stop, line.opts.rounds),
            (Some(3), Some(300), Some(3))
        );
        assert_eq!((line.opts.draw_black, line.opts.draw_white), (Some(-70), Some(70)));
        // main 5i4h は 1 回の読みの値。
        let (main, lines) = propagated_switch(
            &[
                (ROOT, "5i4h", "main", "10"),
                (ROOT, "5i6h", "alt", "20"),
                (c, "5a4a", "main", "-16"),
            ],
            false,
        );
        assert_eq!(main, "5i4h");
        assert_eq!(lines[0].game, vec![vec!["5i4h".to_owned()]]);
    }

    #[test]
    fn single_queue_uses_walk_weights_instead_of_frequency_weights() {
        use crate::book_tree::reply_graph::HIRATE_KEY;
        let after = crate::book_tree::model::child(HIRATE_KEY, "2g2f").unwrap();
        let run_with = |min_weight: f64| {
            let dir = tempfile::tempdir().unwrap();
            let mut args = test_args(dir.path());
            // main 2g2f は先の局面から、1 位 7g7f は 1 回の読み。
            std::fs::write(
                &args.master,
                master(&[
                    (HIRATE_KEY, "2g2f", "main", "10"),
                    (HIRATE_KEY, "7g7f", "alt", "30"),
                    (&after, "8c8d", "main", "-10"),
                ]),
            )
            .unwrap();
            let freq = dir.path().join("freq.db");
            std::fs::write(&freq, "#YANEURAOU-DB2016 1.00\n").unwrap();
            let walk = dir.path().join("walk.tsv");
            std::fs::write(&walk, "side\tweight\tmoves\nb\t0.25\t\nw\t0.5\t\n").unwrap();
            let queue = dir.path().join("single.jsonl");
            args.switch_rule = SwitchRule::Propagated;
            args.single.single_enqueue_out = Some(queue.clone());
            // 頻度の重みは 1 以下なので、--min-weight 2 が効けば行が落ちる。
            args.weight.opp_freq = Some(freq);
            args.weight.min_weight = 2.0;
            args.single.walk_table = vec![walk];
            args.single.single_min_weight = min_weight;
            run_in_pool(args).unwrap();
            std::fs::read_to_string(queue).unwrap()
        };
        let text = run_with(0.25);
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("single=7g7f weight=0.25000\""), "{text}");
        assert_eq!(run_with(0.375), "");
    }

    #[test]
    fn switch_rule_rejects_options_of_the_other_rule() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args(dir.path());
        std::fs::write(&args.master, master(&[(ROOT, "5i4h", "main", "10")])).unwrap();
        args.single.single_enqueue_out = Some(dir.path().join("single.jsonl"));
        assert!(run_in_pool(args).is_err());
        let mut args = test_args(dir.path());
        args.switch_rule = SwitchRule::Propagated;
        args.deep_plies = Some(6);
        assert!(run_in_pool(args).is_err());
    }

    #[test]
    fn propagated_rule_treats_a_bridged_read_as_a_single_read() {
        // 補った A では、定跡の応手 5a5b の先 B (+100) より着く手の読みの値 +50 が悪いので、5i4h の値は読みそのもの。
        let c = "4k4/9/9/9/9/9/9/3K5/9 w -";
        let rows = [
            (ROOT, "5i6h", "main", "10"),
            (ROOT, "5i4h", "alt", "50"),
            (B, "4h3h", "main", "100"),
            (c, "5a4a", "main", "-20"),
        ];
        let (main, lines) = propagated_switch(&rows, true);
        assert_eq!(main, "5i6h");
        assert_eq!(lines[0].game, vec![vec!["5i4h".to_owned()]]);
        // B の値が読みより悪ければ、5i4h の値は応手の先から来るので替える。
        let mut worse = rows;
        worse[2].3 = "30";
        let (main, lines) = propagated_switch(&worse, true);
        assert_eq!(main, "5i4h");
        assert!(lines.is_empty());
    }

    #[test]
    fn propagated_rule_counts_a_record_reply_as_a_value_from_the_child() {
        // A は record だけの局面。record 5a4a の読みが着く手の読み (+10) より A の手番に良ければ、5i4h の値は
        // A の先の値なので、両方とも伝播の値として替える。
        let c = "4k4/9/9/9/9/9/9/3K5/9 w -";
        let rows = [
            (ROOT, "5i4h", "main", "10"),
            (ROOT, "5i6h", "alt", "20"),
            (A, "5a4a", "record", "-5"),
            (c, "5a4a", "main", "-16"),
        ];
        let (main, lines) = propagated_switch_with(&rows, false, true);
        assert_eq!(main, "5i6h");
        assert!(lines.is_empty());
        // record が悪ければ 5i4h の値は着く手の読みそのもの。
        let mut worse = rows;
        worse[2].3 = "-20";
        let (main, lines) = propagated_switch_with(&worse, false, true);
        assert_eq!(main, "5i4h");
        assert_eq!(lines[0].game, vec![vec!["5i4h".to_owned()]]);
    }

    #[test]
    fn single_queue_skips_positions_already_queued() {
        let read = SingleRead {
            key: ROOT.into(),
            ply: "1".into(),
            main: "5i4h".into(),
            main_value: 10,
            best: "5i5h".into(),
            best_value: 99,
            moves: vec!["5i5h".into()],
            weight: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut args = switch::test_args();
        let after = crate::book_tree::model::child(ROOT, "5i5h").unwrap();
        let root = format!("sfen {ROOT} 1");
        let next = format!("sfen {after} 2");
        for (root, own, depth, game, expected) in [
            (&root, "b", 6, "", 0),
            (&root, "b", 6, r#","game":[["5i5h","5a5b"]]"#, 1),
            (&root, "b", 1, r#","game":[["5i5h","5a5b"]]"#, 1),
            (&root, "b", 0, r#","game":[["5i5h"]]"#, 0),
            (&root, "b", 6, r#","game":[["5i4h"]]"#, 0),
            (&root, "w", 6, r#","game":[["5i5h"]]"#, 0),
            (&next, "b", 6, "", 1),
            (&next, "w", 6, "", 0),
        ] {
            let path = dir.path().join("queue.jsonl");
            std::fs::write(
                &path,
                format!(r#"{{"id":"x","root":"{root}","own":"{own}","depth":{depth}{game}}}"#)
                    + "\n",
            )
            .unwrap();
            args.exclude_queue = vec![path];
            let (lines, excluded) = switch::queue(std::slice::from_ref(&read), &args).unwrap();
            assert_eq!((lines.len(), excluded), (1 - expected, expected), "{root} {own} {game}");
        }
        // 段数 depth の局面は伸ばさないので、game の depth+1 手目は除外に使わない。
        let deep = SingleRead {
            key: B.into(),
            moves: vec!["4h3h".into()],
            ..read.clone()
        };
        for (depth, expected) in [(2, 0), (3, 1)] {
            std::fs::write(
                &args.exclude_queue[0],
                format!(
                    r#"{{"id":"x","root":"{root}","own":"b","depth":{depth},"game":[["5i4h","5a5b","4h3h"]]}}"#
                ) + "\n",
            )
            .unwrap();
            let (_, excluded) = switch::queue(std::slice::from_ref(&deep), &args).unwrap();
            assert_eq!(excluded, expected, "depth {depth}");
        }
        // id ごとに最初の行の根に固定するので、根を替えた行は除外に使わない。
        std::fs::write(
            &args.exclude_queue[0],
            format!(
                "{}\n{}\n",
                r#"{"id":"x","root":"ROOTW","own":"w","depth":6}"#.replace("ROOTW", &next),
                r#"{"id":"x","root":"ROOTB","own":"b","depth":6,"game":[["5i5h"]]}"#
                    .replace("ROOTB", &root)
            ),
        )
        .unwrap();
        assert_eq!(switch::queue(std::slice::from_ref(&read), &args).unwrap().1, 0);
        // 両方が 1 回の読みなら、積んでいない手だけを行に入れる。
        std::fs::write(
            &args.exclude_queue[0],
            format!(r#"{{"id":"x","root":"{next}","own":"b","depth":6}}"#) + "\n",
        )
        .unwrap();
        let both = SingleRead {
            moves: vec!["5i4h".into(), "5i5h".into()],
            ..read
        };
        let (lines, excluded) = switch::queue(std::slice::from_ref(&both), &args).unwrap();
        assert_eq!(excluded, 0);
        assert_eq!(lines[0].game, vec![vec!["5i4h".to_owned()]]);
        args.exclude_queue.clear();
        args.single_max = Some(0);
        assert!(switch::queue(&[both], &args).unwrap().0.is_empty());
    }

    #[test]
    fn bridge_replies_does_not_follow_flipped_children() {
        // ROOT の 5i4h の直接の子局面 A は無く、反転した局面だけがある。補わない伝播は反転した局面の値を使い、
        // 補う伝播は反転の辺を張らずに読みの値 +50 を使う。
        let flipped = rshogi_book::flipped_key(&format!("{A} 1")).unwrap();
        let flipped_a = strip_ply(&flipped).to_owned();
        let rows = [
            (ROOT, "5i4h", "main", "50"),
            (flipped_a.as_str(), "5i5h", "main", "100"),
        ];
        let parsed = parse_master_rows(master(&rows).as_bytes()).unwrap();
        let main = |fill: Fill| {
            let (_, _, book) = audit(&parsed, DrawValues::both(0), 1, fill, None).unwrap();
            let index = parsed.0.keys().position(|k| k == ROOT).unwrap();
            audited_moves(&book.graph, ROOT, &parsed.0[ROOT], &book.known[index]).unwrap()[0]
                .as_ref()
                .map(|m| m.value)
        };
        assert_eq!(main(Fill::Off), Some(-100));
        assert_eq!(main(Fill::Bridge), Some(50));
    }

    #[test]
    fn bridge_replies_does_not_fill_after_unread_moves() {
        // 読みの値の無い 5i4h の行き先 A は補わない (補うと定跡の応手だけの甘い値になる)。
        let (summary, main) =
            bridge_audit(&[(ROOT, "5i4h", "main", ""), (B, "4h3h", "main", "50")]);
        assert_eq!((summary.reply_positions, main), (0, None));
    }

    #[test]
    fn bridge_replies_revive_does_not_use_flipped_child() {
        // bench 5i4h の直接の子局面は無く、反転した局面だけが元データにある。補う伝播は反転の辺を張らないので、
        // 復帰の判定も読みの値 -999 を使い、bench のまま残す。補わなければ反転した局面の値で alt に戻す。
        let rows = [
            (ROOT, "5i6h", "main", "0"),
            (ROOT, "5i4h", "bench", "-999"),
            (A_FLIPPED, "5i5h", "main", "-10"),
        ];
        for (bridge, expected) in [(true, "bench"), (false, "alt")] {
            let dir = tempfile::tempdir().unwrap();
            let mut args = test_args(dir.path());
            std::fs::write(&args.master, master(&rows)).unwrap();
            args.bridge_replies = bridge;
            args.revive_bench = true;
            args.apply = true;
            args.since = Some("t2".into());
            run_in_pool(args).unwrap();
            let text = std::fs::read_to_string(dir.path().join("moves.tsv")).unwrap();
            let role = super::super::master_table::parse_master(&text)
                .unwrap()
                .iter()
                .find(|r| r.get("key") == ROOT && r.get("move") == "5i4h")
                .map(|r| r.get("role").to_owned())
                .unwrap();
            assert_eq!(role, expected, "bridge {bridge}");
        }
    }

    #[test]
    fn bridge_table_lists_worsened_mains() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args(dir.path());
        std::fs::write(
            &args.master,
            master(&[(ROOT, "5i4h", "main", "50"), (B, "4h3h", "main", "-100")]),
        )
        .unwrap();
        args.bridge_replies = true;
        args.bridge_table = Some(dir.path().join("bridge.tsv"));
        args.draw_value_black = 0;
        args.line = 40;
        run_in_pool(args).unwrap();
        let text = std::fs::read_to_string(dir.path().join("bridge.tsv")).unwrap();
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows.len(), 2, "{text}");
        assert_eq!(rows[1], format!("{ROOT}\t1\tb\t5i4h\t50\t-100\t150\t5i4h\t-100\t50\t-100\t"));
    }

    fn bridge_row(key: &str, main: &str, drop: i32) -> BridgeFinding {
        BridgeFinding {
            key: key.into(),
            ply: "7".into(),
            main: main.into(),
            plain: 50,
            bridged: 50 - drop,
            best: main.into(),
            best_value: 50 - drop,
            weight: None,
        }
    }

    #[test]
    fn bridge_queue_preserves_root_side_main_and_options() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args(dir.path());
        args.bridge_depth = 8;
        args.bridge_priority = 321;
        args.bridge_opp_n = Some(3);
        args.bridge_stop = Some(90);
        args.bridge_rounds = Some(2);
        args.bridge_draw_black = Some(-70);
        args.bridge_draw_white = Some(-20);
        for (key, mv, side, id) in [
            (ROOT, "5i4h", "b", "18064834fcf08122_bridge_b"),
            (A, "5a5b", "w", "8c610a1b413a2d6e_bridge_w"),
        ] {
            let row = bridge_row(key, mv, 150);
            let line = bridge_queue_line(&row, 2, &args).unwrap();
            assert_eq!(line.root, format!("sfen {key} 7"));
            assert_eq!(line.own, side);
            assert_eq!(line.game, vec![vec![mv]]);
            assert!(line.roots.is_empty());
            assert!(!line.hold);
            assert_eq!((line.depth, line.priority), (8, 323));
            assert_eq!(line.id, id);
            assert_eq!(line.opts, args.bridge_opts());
            assert_eq!(
                line.memo.as_deref(),
                Some(
                    format!("bridge main={mv} plain=50 bridged=-100 best={mv} best_value=-100")
                        .as_str()
                )
            );
            assert_eq!(bridge_queue_line(&row, 100, &args).unwrap().priority, 420);
            line.check().unwrap();
        }
    }

    #[test]
    fn bridge_queue_excludes_latest_active_roots_before_ranking_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args(dir.path());
        let rows = vec![bridge_row(ROOT, "5i4h", 150), bridge_row(B, "4h3h", 100)];
        let mut line = bridge_queue_line(&rows[0], 0, &args).unwrap();
        line.roots = vec![format!("sfen {B} 3")];
        let first = dir.path().join("first.jsonl");
        let last = dir.path().join("last.jsonl");
        std::fs::write(&first, refutation::format_queue(&[line.clone()]).unwrap()).unwrap();
        line.hold = true;
        std::fs::write(&last, refutation::format_queue(&[line.clone()]).unwrap()).unwrap();
        args.bridge_exclude_queue = vec![first.clone(), last.clone()];
        assert_eq!(bridge_queue(&rows, &args).unwrap().0.len(), 2);
        args.bridge_exclude_queue = vec![last, first.clone()];
        let (lines, excluded) = bridge_queue(&rows, &args).unwrap();
        assert!(lines.is_empty());
        assert_eq!(excluded, 2);
        line.hold = false;
        line.roots.clear();
        std::fs::write(&first, refutation::format_queue(&[line.clone()]).unwrap()).unwrap();
        args.bridge_exclude_queue = vec![first.clone()];
        args.bridge_max = Some(1);
        let (lines, excluded) = bridge_queue(&rows, &args).unwrap();
        assert_eq!(excluded, 1);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].root.contains(B));
        assert_eq!(lines[0].priority, DEFAULT_PRIORITY);
        for (depth, own) in [(5, "b"), (6, "w")] {
            line.depth = depth;
            line.own = own.into();
            std::fs::write(&first, refutation::format_queue(&[line.clone()]).unwrap()).unwrap();
            assert_eq!(bridge_queue(&rows, &args).unwrap().1, 0);
        }
    }

    #[test]
    fn bridge_weights_sort_by_side_weight_drop_and_key_and_filter_table_and_queue() {
        use std::collections::HashMap;
        let dir = tempfile::tempdir().unwrap();
        let args = test_args(dir.path());
        let input = vec![
            bridge_row(ROOT, "5i4h", 150),
            bridge_row(A, "5a5b", 100),
            bridge_row(B, "4h3h", 100),
        ];
        let weights: SideWeights = [
            HashMap::from([(ROOT.into(), 0.2), (A.into(), 99.0), (B.into(), 0.5)]),
            HashMap::from([(A.into(), 0.5), (ROOT.into(), 99.0)]),
        ];
        let mut rows = input.clone();
        weight_bridge_findings(&mut rows, Some(&weights), 0.2);
        let mut tied = [A, B];
        tied.sort();
        assert_eq!(
            rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            [tied[0], tied[1], ROOT]
        );
        rows.iter_mut().find(|r| r.key == B).unwrap().bridged = -200;
        weight_bridge_findings(&mut rows, Some(&weights), 0.3);
        assert_eq!(rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), [B, A]);
        let table = format_bridge_table(&rows);
        assert!(table.lines().next().unwrap().ends_with("\tweight"));
        assert!(table.lines().skip(1).all(|r| r.ends_with("\t0.500000")));
        assert_eq!(bridge_queue(&rows, &args).unwrap().0.len(), 2);
        weight_bridge_findings(&mut rows, Some(&weights), 0.6);
        assert_eq!(format_bridge_table(&rows).lines().count(), 1);
        assert!(bridge_queue(&rows, &args).unwrap().0.is_empty());
        let mut rows = input;
        weight_bridge_findings(&mut rows, None, 1.0);
        assert_eq!(rows[0].key, ROOT);
        assert_eq!(rows.len(), 3);
        assert!(format_bridge_table(&rows).lines().skip(1).all(|r| r.ends_with('\t')));
    }

    #[test]
    fn bridge_enqueue_requires_bridge_replies_and_valid_options() {
        #[derive(clap::Parser, Debug)]
        struct Cli {
            #[command(flatten)]
            args: AuditArgs,
        }
        use clap::Parser;
        let base = [
            "audit",
            "--master",
            "moves.tsv",
            "--table",
            "audit.tsv",
            "--bridge-enqueue-out",
            "queue.jsonl",
        ];
        assert_eq!(
            Cli::try_parse_from(base).unwrap_err().kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        let args = Cli::try_parse_from(base.into_iter().chain([
            "--bridge-replies",
            "--bridge-draw-black",
            "-70",
            "--bridge-draw-white",
            "-10",
        ]))
        .unwrap()
        .args;
        assert!(args.bridge_table.is_none());
        assert_eq!(args.bridge_depth, 6);
        assert_eq!(args.bridge_priority, DEFAULT_PRIORITY);
        assert_eq!(args.bridge_exclude_min_depth, 6);
        args.bridge_opts().check().unwrap();
        for extra in [["--bridge-depth", "999"], ["--bridge-priority", "1000000"]] {
            assert!(
                Cli::try_parse_from(base.into_iter().chain(["--bridge-replies"]).chain(extra))
                    .is_err()
            );
        }
        let args = Cli::try_parse_from(base.into_iter().chain([
            "--bridge-replies",
            "--bridge-stop",
            "-1",
        ]))
        .unwrap()
        .args;
        assert!(args.bridge_opts().check().is_err());
    }

    #[test]
    fn bridge_outputs_accept_frequency_alone_and_with_refutation() {
        let dir = tempfile::tempdir().unwrap();
        let freq = dir.path().join("freq.db");
        std::fs::write(&freq, "#YANEURAOU-DB2016 1.00\n").unwrap();
        std::fs::write(
            dir.path().join("moves.tsv"),
            master(&[(ROOT, "5i4h", "main", "50"), (B, "4h3h", "main", "-100")]),
        )
        .unwrap();
        for (table, queue, refutation) in [
            (true, false, false),
            (false, true, false),
            (true, true, true),
        ] {
            let mut args = test_args(dir.path());
            args.bridge_replies = true;
            args.weight.opp_freq = Some(freq.clone());
            args.bridge_table = table.then(|| dir.path().join("bridge.tsv"));
            args.bridge_enqueue_out = queue.then(|| dir.path().join("bridge.jsonl"));
            args.refutation_table = refutation.then(|| dir.path().join("refutation.tsv"));
            run_in_pool(args).unwrap();
            if table {
                assert!(
                    std::fs::read_to_string(dir.path().join("bridge.tsv"))
                        .unwrap()
                        .lines()
                        .nth(1)
                        .unwrap()
                        .ends_with("\t0.000000")
                );
            }
            if queue {
                assert_eq!(
                    std::fs::read_to_string(dir.path().join("bridge.jsonl"))
                        .unwrap()
                        .lines()
                        .count(),
                    1
                );
            }
        }
    }

    #[test]
    fn bridge_enqueue_works_without_bridge_table_and_rejects_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args(dir.path());
        std::fs::write(
            &args.master,
            master(&[(ROOT, "5i4h", "main", "50"), (B, "4h3h", "main", "-100")]),
        )
        .unwrap();
        args.bridge_replies = true;
        let out = dir.path().join("bridge.jsonl");
        args.bridge_enqueue_out = Some(out.clone());
        run_in_pool(args).unwrap();
        let mut count = 0;
        read_checked_lines(&out, |line, _, _| {
            assert_eq!(line.root, format!("sfen {ROOT} 1"));
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
        for collision in [
            out.clone(),
            dir.path().join("audit.tsv"),
            dir.path().join("moves.tsv"),
        ] {
            let mut args = test_args(dir.path());
            args.bridge_replies = true;
            args.bridge_enqueue_out = Some(collision);
            args.bridge_exclude_queue = vec![out.clone()];
            assert!(run_in_pool(args).is_err());
        }
    }

    #[test]
    fn bad_bridge_exclude_queue_writes_no_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args(dir.path());
        std::fs::write(
            &args.master,
            master(&[(ROOT, "5i4h", "main", "50"), (B, "4h3h", "main", "-100")]),
        )
        .unwrap();
        args.bridge_replies = true;
        args.bridge_table = Some(dir.path().join("bridge.tsv"));
        args.bridge_enqueue_out = Some(dir.path().join("bridge.jsonl"));
        args.bridge_exclude_queue = vec![dir.path().join("missing.jsonl")];
        assert!(run_in_pool(args).is_err());
        for name in ["audit.tsv", "bridge.tsv", "bridge.jsonl"] {
            assert!(!dir.path().join(name).exists(), "{name}");
        }
    }

    #[test]
    fn bridge_queue_line_rejects_rows_without_ply() {
        let dir = tempfile::tempdir().unwrap();
        let args = test_args(dir.path());
        for ply in ["", "0"] {
            let mut row = bridge_row(ROOT, "5i4h", 150);
            row.ply = ply.into();
            let err = bridge_queue_line(&row, 0, &args).unwrap_err();
            assert!(format!("{err:#}").contains("手数"), "{err:#}");
        }
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
        let replies = reply_positions(&positions, Fill::Unread).unwrap();
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
            revive_bench: false,
            bad_eval: -150,
            since: None,
            schema: None,
            table: dir.join("audit.tsv"),
            line: 1,
            switch_rule: SwitchRule::Line,
            single: switch::test_args(),
            deep_plies: None,
            deep: DeepArgs {
                deep_opp_freq: None,
                deep_min_games: 3,
            },
            demote_line: 100,
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
            bridge_replies: false,
            opp_records: false,
            bridge_table: None,
            bridge_enqueue_out: None,
            bridge_depth: 6,
            bridge_max: None,
            bridge_priority: DEFAULT_PRIORITY,
            bridge_opp_n: None,
            bridge_stop: None,
            bridge_rounds: None,
            bridge_draw_black: None,
            bridge_draw_white: None,
            bridge_exclude_queue: Vec::new(),
            bridge_exclude_min_depth: 6,
            weight: WeightArgs {
                opp_freq: None,
                min_games: 3,
                max_ply: 80,
                weight_floor: 1e-5,
                min_weight: 0.0,
                opp_book: None,
                opp_book_diff: 30,
                opp_book_best: 0.9,
                opp_book_first: false,
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
        let (findings, _, _) = audit(&positions, DrawValues::both(0), 1, Fill::Off, None).unwrap();
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
        args.demote_line = 40;
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
    fn bench_revival_respects_mover_draw_in_asymmetric_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let c = super::super::model::child(B, "4h3h").unwrap();
        let d = super::super::model::child(&c, "5b5a").unwrap();
        let text = master(&[
            (ROOT, "5i6h", "main", "-30"),
            (ROOT, "5i4h", "bench", "999"),
            (A, "5a6a", "main", "30"),
            (A, "5a5b", "alt", "0"),
            (B, "4h5h", "main", "-50"),
            (B, "4h3h", "alt", "0"),
            (&c, "5b5a", "main", "0"),
            (&d, "3h4h", "main", "0"),
        ]);
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let draws = DrawValues {
            black: -70,
            white: 0,
        };
        let (_, _, book) = audit(&positions, draws, 15, Fill::Off, None).unwrap();
        let mut rows = parse_master(&text).unwrap();
        let target = bench_target(&rows[1], &book.graph).unwrap().unwrap();
        let naive = -book
            .graph
            .moves(target.0)
            .filter(|m| m.usable)
            .map(|m| m.new + i32::from(m.tie_lowered))
            .max()
            .unwrap();
        assert_eq!(naive, -30);
        assert_eq!(evaluate_edges(&book.graph, draws, &[target]).unwrap(), [-50]);
        let mut args = test_args(dir.path());
        args.draw_value_black = -70;
        args.draw_value_white = 0;
        args.demote_line = 15;
        let summary =
            revive_bench(&mut rows, &positions.0, &book, &Schema::default(), &args).unwrap();
        assert_eq!(summary.revived, [0, 0]);
        assert_eq!(rows[1].get("role"), "bench");
    }

    #[test]
    fn cli_separates_replacement_and_demotion_thresholds() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            args: AuditArgs,
        }
        use clap::Parser;
        let base = ["audit", "--master", "moves.tsv", "--table", "audit.tsv"];
        let defaults = Cli::try_parse_from(base).unwrap().args;
        assert_eq!((defaults.line, defaults.demote_line), (40, 100));
        assert!(!defaults.opp_records);
        let combined = Cli::try_parse_from(base.into_iter().chain([
            "--opp-records",
            "--bridge-replies",
            "--deep-plies",
            "2",
            "--deep-opp-freq",
            "frequency.db",
            "--revive-bench",
            "--demote-alts",
        ]))
        .unwrap()
        .args;
        assert!(combined.opp_records);
        assert!(combined.bridge_replies);

        let custom = Cli::try_parse_from(base.into_iter().chain(["--demote-line", "70"]))
            .unwrap()
            .args;
        assert_eq!((custom.line, custom.demote_line), (40, 70));
    }

    #[test]
    fn cli_deep_plies_requires_positive_value() {
        #[derive(clap::Parser, Debug)]
        struct Cli {
            #[command(flatten)]
            args: AuditArgs,
        }
        use clap::Parser;
        let base = ["audit", "--master", "moves.tsv", "--table", "audit.tsv"];
        assert!(Cli::try_parse_from(base).unwrap().args.deep_plies.is_none());
        assert_eq!(
            Cli::try_parse_from(base.into_iter().chain(["--deep-plies", "0"]))
                .unwrap_err()
                .kind(),
            clap::error::ErrorKind::ValueValidation
        );
        let args = Cli::try_parse_from(base.into_iter().chain(["--deep-plies", "1"])).unwrap().args;
        assert_eq!(args.deep_plies, Some(1));
    }

    #[test]
    fn standalone_revival_dry_run_matches_apply_with_default_demote_line() {
        let dir = tempfile::tempdir().unwrap();
        let text = master(&[
            (ROOT, "5i6h", "main", "100"),
            (ROOT, "5i4h", "bench", "1"),
            (ROOT, "5i5h", "bench", "0"),
        ]);
        for apply in [false, true] {
            let mut args = test_args(dir.path());
            std::fs::write(&args.master, &text).unwrap();
            args.revive_bench = true;
            args.apply = apply;
            args.since = Some("tag".into());
            args.line = 40;
            let result = run_with_revivals(args).unwrap();
            assert_eq!(result.revived, [1, 0]);
            let saved = std::fs::read_to_string(dir.path().join("moves.tsv")).unwrap();
            if apply {
                let rows = parse_master(&saved).unwrap();
                assert_eq!(rows[1].get("role"), "alt");
                assert_eq!(rows[2].get("role"), "bench");
            } else {
                assert_eq!(saved, text);
            }
        }
    }

    #[test]
    fn curated_bench_without_curated_main_is_counted_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let text = master(&[
            (ROOT, "5i6h", "main", "0"),
            (ROOT, "5i4h", "bench", "0"),
            (ROOT, "5i5h", "record", "0"),
        ]);
        let mut rows = parse_master(&text).unwrap();
        rows[1].set("tier", "curated".into());
        rows[2].set("tier", "curated".into());
        let text = format_master(&rows);
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let (_, _, book) = audit(&positions, DrawValues::both(0), 40, Fill::Off, None).unwrap();
        let mut args = test_args(dir.path());
        args.replace_curated = true;
        let summary =
            revive_bench(&mut rows, &positions.0, &book, &Schema::default(), &args).unwrap();
        assert_eq!(summary.revived, [0, 0]);
        assert_eq!(summary.curated_without_main, 1);
        master_table::validate(&rows).unwrap();
        std::fs::write(&args.master, &text).unwrap();
        args.revive_bench = true;
        args.apply = true;
        args.since = Some("tag".into());
        run(args).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("moves.tsv")).unwrap(), text);
    }

    #[test]
    fn bench_revival_thresholds_protection_and_apply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        for (main, bench, child, expected) in [
            ("100", "61", None, "alt"),
            ("100", "60", None, "bench"),
            ("-140", "-150", None, "bench"),
            ("100", "", None, "bench"),
            ("100", "999", Some("0"), "bench"),
            ("100", "-999", Some("-100"), "alt"),
        ] {
            for apply in [false, true] {
                for enabled in [false, true] {
                    let mut input =
                        vec![(ROOT, "5i6h", "main", main), (ROOT, "5i4h", "bench", bench)];
                    if let Some(child) = child {
                        input.push((A, "5a5b", "main", child));
                    }
                    let text = master(&input);
                    std::fs::write(&path, &text).unwrap();
                    let mut args = test_args(dir.path());
                    args.apply = apply;
                    args.revive_bench = enabled;
                    args.demote_alts = true;
                    args.line = 40;
                    args.demote_line = 40;
                    args.since = Some("tag".into());
                    run(args).unwrap();
                    let updated = parse_master(&std::fs::read_to_string(&path).unwrap()).unwrap();
                    assert_eq!(
                        updated[1].get("role"),
                        if apply && enabled { expected } else { "bench" }
                    );
                    if updated[1].get("role") == "alt" {
                        assert!(
                            updated[1]
                                .get("note")
                                .starts_with("tag: 値が戻ったので bench から alt (")
                        );
                    }
                }
            }
        }
        let text = master(&[
            (ROOT, "5i6h", "main", "100"),
            (ROOT, "5i4h", "bench", "100"),
        ]);
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let (_, _, book) = audit(&positions, DrawValues::both(0), 40, Fill::Off, None).unwrap();
        for curated in [false, true] {
            let mut rows = parse_master(&text).unwrap();
            if curated {
                rows[0].set("tier", "curated".into());
            } else {
                rows[1].set("source", "protected".into());
            }
            let mut args = test_args(dir.path());
            args.line = 40;
            args.demote_line = 40;
            args.replace_curated = false;
            assert_eq!(
                revive_bench(&mut rows, &positions.0, &book, &Schema::default(), &args)
                    .unwrap()
                    .revived,
                [0, 0]
            );
            if curated {
                args.replace_curated = true;
                assert_eq!(
                    revive_bench(&mut rows, &positions.0, &book, &Schema::default(), &args)
                        .unwrap()
                        .revived,
                    [1, 0]
                );
            }
        }
    }

    #[test]
    fn bench_revival_uses_flipped_children_and_counts_both_sides() {
        let dir = tempfile::tempdir().unwrap();
        let white = "4k4/9/9/9/9/9/9/9/4K4 w -";
        let text = master(&[
            (ROOT, "5i6h", "main", "0"),
            (ROOT, "5i4h", "bench", "-999"),
            (A_FLIPPED, "5i5h", "main", "-10"),
            (white, "5a6b", "main", "0"),
            (white, "5a4b", "bench", "10"),
        ]);
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let (_, _, book) = audit(&positions, DrawValues::both(0), 40, Fill::Off, None).unwrap();
        let mut rows = parse_master(&text).unwrap();
        assert_eq!(
            evaluate_edges(
                &book.graph,
                DrawValues::both(0),
                &[bench_target(&rows[1], &book.graph).unwrap().unwrap()]
            )
            .unwrap(),
            [10]
        );
        let result = revive_bench(
            &mut rows,
            &positions.0,
            &book,
            &Schema::default(),
            &test_args(dir.path()),
        )
        .unwrap()
        .revived;
        assert_eq!(result, [1, 1]);
    }

    #[test]
    fn bench_revival_reference_excludes_direct_alt_and_requires_known_main_or_expansion() {
        let dir = tempfile::tempdir().unwrap();
        for (main, expected) in [("0", [1, 0]), ("", [0, 0])] {
            let text = master(&[
                (ROOT, "5i6h", "main", main),
                (ROOT, "5i5h", "alt", "9999"),
                (ROOT, "5i4h", "bench", "0"),
            ]);
            let positions = parse_master_rows(text.as_bytes()).unwrap();
            let (_, _, book) = audit(&positions, DrawValues::both(0), 40, Fill::Off, None).unwrap();
            let mut rows = parse_master(&text).unwrap();
            assert_eq!(
                revive_bench(
                    &mut rows,
                    &positions.0,
                    &book,
                    &Schema::default(),
                    &test_args(dir.path())
                )
                .unwrap()
                .revived,
                expected
            );
        }
    }

    #[test]
    fn bench_does_not_change_playable_values() {
        let text = master(&[
            (ROOT, "5i6h", "main", "10"),
            (ROOT, "5i4h", "bench", "9999"),
            (A, "5a5b", "main", "-9999"),
        ]);
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let values = master_values(&positions, DrawValues::both(0)).unwrap();
        assert_eq!(values[ROOT].best, Some(10));
        let mut rows = parse_master(&text).unwrap();
        rows.retain(|r| r.get("role") != "bench");
        let without = parse_master_rows(format_master(&rows).as_bytes()).unwrap();
        assert_eq!(values, master_values(&without, DrawValues::both(0)).unwrap());
    }

    #[test]
    fn demotions_use_values_before_tie_adjustment_and_keep_other_columns() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (-140, -150, 40, "bench", [1, 0], [0, 0]),
            (100, 60, 40, "bench", [0, 0], [1, 0]),
            (100, 60, 100, "alt", [0, 0], [0, 0]),
            (100, 30, 100, "alt", [0, 0], [0, 0]),
            (100, 1, 100, "alt", [0, 0], [0, 0]),
            (100, 0, 100, "bench", [0, 0], [1, 0]),
            (100, 100, 1, "alt", [0, 0], [0, 0]),
            (-200, -200, 1, "alt", [0, 0], [0, 0]),
            (-200, -210, 40, "bench", [1, 0], [0, 0]),
            (-200, -240, 40, "bench", [0, 0], [1, 0]),
        ];
        for (main, alt, line, role, bad, gap) in cases {
            let text = master(&[
                (ROOT, "5i6h", "main", &main.to_string()),
                (ROOT, "5i4h", "alt", &alt.to_string()),
                (ROOT, "5i5h", "record", "-999"),
                (ROOT, "5i4i", "alt", ""),
            ]);
            let positions = parse_master_rows(text.as_bytes()).unwrap();
            let (_, _, book) =
                audit(&positions, DrawValues::both(0), line, Fill::Off, None).unwrap();
            let original = parse_master(&text).unwrap();
            let mut rows = original.clone();
            let mut args = test_args(dir.path());
            args.demote_line = line;
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
            if role == "bench" {
                assert_eq!(
                    rows[1].get("note"),
                    format!(
                        "tag: 伝播後の値が悪いので alt から bench (控え) (基準との差 {} / 値 {alt} / demote-line {line} / bad-eval -150)",
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
        let (findings, _, book) =
            audit(&positions, DrawValues::both(0), 40, Fill::Off, None).unwrap();
        let mut rows = parse_master(&text).unwrap();
        let mut args = test_args(dir.path());
        args.line = 40;
        args.demote_line = 40;
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
        args.demote_line = 40;
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
                let (_, _, book) =
                    audit(&positions, DrawValues::both(0), 1, Fill::Off, None).unwrap();
                let result =
                    demote_alts(&mut rows, &positions.0, &book, &Schema::default(), &args).unwrap();
                assert_eq!(result.skipped_curated, usize::from(!replace_curated));
                run(args).unwrap();
                let updated = parse_master(&std::fs::read_to_string(&path).unwrap()).unwrap();
                assert_eq!(updated, rows);
                assert_eq!(updated[1].get("role"), if replace_curated { "bench" } else { "alt" });
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
                let (_, _, book) =
                    audit(&positions, DrawValues::both(0), 40, Fill::Off, None).unwrap();
                let mut rows = parse_master(&text).unwrap();
                let original = rows.clone();
                let mut args = test_args(dir.path());
                args.line = 40;
                args.demote_line = 40;
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
        let (_, _, book) = audit(&positions, DrawValues::both(0), 40, Fill::Off, None).unwrap();
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
        args.revive_bench = true;
        args.line = 40;
        args.demote_line = 40;
        run(args).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let positions = parse_master_rows(text.as_bytes()).unwrap();
        let (_, _, book) = audit(&positions, DrawValues::both(0), 40, Fill::Off, None).unwrap();
        let mut rows = parse_master(&text).unwrap();
        let mut args = test_args(dir.path());
        args.demote_alts = true;
        args.revive_bench = true;
        args.line = 40;
        args.demote_line = 40;
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
        let mut deep_freq_only = args("audit2.tsv", None);
        deep_freq_only.deep.deep_opp_freq = Some(dir.path().join("freq.db"));
        assert!(run(deep_freq_only).unwrap_err().to_string().contains("--deep-plies"));
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

    #[test]
    fn revive_bench_does_not_return_needless_unpromotions() {
        const W: &str = "4k4/7b1/9/9/9/9/9/9/4K4 w -";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        let text = master(&[
            (W, "2b3c", "main", "45"),
            (W, "2b7g", "bench", "40"),
            (W, "2b6f", "bench", "40"),
        ]);
        std::fs::write(&path, &text).unwrap();
        let mut args = test_args(dir.path());
        args.revive_bench = true;
        args.apply = true;
        args.since = Some("T".into());
        run(args).unwrap();
        let rows = parse_master(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let roles: Vec<_> = rows.iter().map(|r| (r.get("move"), r.get("role"))).collect();
        assert_eq!(roles, [("2b3c", "main"), ("2b7g", "bench"), ("2b6f", "alt")]);
    }

    #[test]
    fn apply_demotes_needless_unpromotions_and_reselects_the_main() {
        const W: &str = "4k4/7b1/9/9/8r/p8/9/9/4K4 w -";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        let text = master(&[
            (W, "2b7g", "main", "50"),
            (W, "2b3c", "alt", "45"),
            (W, "2b7g+", "alt", "40"),
            (W, "1e1h", "alt", "60"),
        ]);
        std::fs::write(&path, &text).unwrap();
        let mut args = test_args(dir.path());
        args.switch_rule = SwitchRule::Propagated;
        run(args).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let mut args = test_args(dir.path());
        args.switch_rule = SwitchRule::Propagated;
        args.apply = true;
        args.since = Some("T".into());
        run(args).unwrap();
        let rows = parse_master(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let roles: Vec<_> = rows.iter().map(|r| (r.get("move"), r.get("role"))).collect();
        assert_eq!(
            roles,
            [
                ("2b7g", "record"),
                ("2b3c", "main"),
                ("2b7g+", "alt"),
                ("1e1h", "record")
            ]
        );
        master_table::validate(&rows).unwrap();
        let report =
            master_table::validate_table(std::fs::read(&path).unwrap().as_slice()).unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }
}
