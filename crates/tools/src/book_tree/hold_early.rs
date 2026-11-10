//! 序盤の既存局面の main を戻し、保留する変更を一覧にする。
use super::{
    deep::{Continuation, Cut, DeepArgs, ReplyFrequency, Stop},
    export::files,
    master_table::{Row, format_master, mean_eval, parse_master},
    refutation::format_queue,
    schema::{self, Schema},
    targets::{DEFAULT_PRIORITY, Line, MAX_DEPTH, MAX_PRIORITY, Opts},
    walk::{check_tag, excluded_positions, queue_id},
};
use crate::book_db::{apply_usi_move, position_from_sfen, strip_ply};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
};

#[derive(Args, Debug)]
pub(super) struct HoldEarlyArgs {
    /// 版を作る前の元データ
    #[arg(long)]
    before: PathBuf,
    /// 登録・監査後の元データ
    #[arg(long)]
    master: PathBuf,
    /// before の main の ply がこの値未満の局面を対象にする
    #[arg(long, default_value_t = 20)]
    below_ply: u64,
    /// 両方の手の後に main が N 手以上続く場合は替わった main を残す (頻度定跡なしでは計数上限 60 手)
    #[arg(long)]
    continuation: Option<usize>,
    #[command(flatten)]
    deep: DeepArgs,
    /// 分類によらず保留する局面キーの一覧 (1 行 1 キー)
    #[arg(long)]
    hold_keys: Option<PathBuf>,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// 書き戻す行の note に付けるタグ
    #[arg(long)]
    since: String,
    /// 保留する変更の一覧 (TSV)
    #[arg(long)]
    out: PathBuf,
    /// 検査後に元データを書き換える
    #[arg(long)]
    apply: bool,
    /// hold の局面の、継続が N 手に届かない旧手・新手の最短の枝が途切れた局面の一覧 (TSV)
    #[arg(long, requires = "continuation")]
    cuts_out: Option<PathBuf>,
    /// 途切れた局面を根にした tree enqueue --from の行 (JSON Lines)。同じ局面・同じ自分の側は 1 行
    #[arg(long, requires = "continuation")]
    cuts_enqueue_out: Option<PathBuf>,
    /// キューの行の段数
    #[arg(long, default_value_t = 6, requires = "cuts_enqueue_out", value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(0..=MAX_DEPTH as u64))]
    cuts_depth: usize,
    /// キューの行の priority の基準。保留の局面の手数の順位 (0 始まり、99 で頭打ち) を足す
    #[arg(long, default_value_t = DEFAULT_PRIORITY, requires = "cuts_enqueue_out", value_parser = clap::value_parser!(u32).range(0..=i64::from(MAX_PRIORITY - 99)))]
    cuts_priority: u32,
    /// キューの行の相手の手の数 (1〜8)
    #[arg(long, requires = "cuts_enqueue_out")]
    cuts_opp_n: Option<usize>,
    /// キューの行の打ち切り値 (1〜30000)
    #[arg(long, requires = "cuts_enqueue_out")]
    cuts_stop: Option<i32>,
    /// キューの行の読み直し回数 (0〜10)
    #[arg(long, requires = "cuts_enqueue_out")]
    cuts_rounds: Option<usize>,
    /// 先手番で循環に入る手の値 (-30000〜30000)
    #[arg(long, requires = "cuts_enqueue_out", allow_hyphen_values = true)]
    cuts_draw_black: Option<i32>,
    /// 後手番で循環に入る手の値 (-30000〜30000)
    #[arg(long, requires = "cuts_enqueue_out", allow_hyphen_values = true)]
    cuts_draw_white: Option<i32>,
    /// キューの行の id `<tag>_<根の手数>_<局面の hash>_<b|w>` に入れる名前 ([A-Za-z0-9_-] の 1〜40 文字)
    #[arg(long, default_value = "held", requires = "cuts_enqueue_out")]
    cuts_tag: String,
    /// 既に積んだ木のキュー (JSON Lines、`master walk --exclude-queue` と同じ除外)。繰り返して複数
    #[arg(long, requires = "cuts_enqueue_out")]
    cuts_exclude_queue: Vec<PathBuf>,
}

impl HoldEarlyArgs {
    fn cuts_opts(&self) -> Opts {
        Opts {
            opp_n: self.cuts_opp_n,
            stop: self.cuts_stop,
            rounds: self.cuts_rounds,
            draw_black: self.cuts_draw_black,
            draw_white: self.cuts_draw_white,
            ..Opts::default()
        }
    }
}

/// hold の局面の旧手か新手の、継続が N 手に届かない枝の途切れ。
struct HeldCut<'a> {
    ply: u64,
    key: &'a str,
    which: &'static str,
    mv: String,
    depth: usize,
    cut: &'a Cut,
}

impl HeldCut<'_> {
    fn side(&self) -> &str {
        self.key.split_whitespace().nth(1).unwrap_or("")
    }

    /// 途切れた局面の手数。保留の局面の手、根の手、途切れまでの手の分だけ進める。
    fn cut_ply(&self) -> u64 {
        self.ply + 1 + self.cut.moves.len() as u64
    }
}

fn format_cuts(cuts: &[HeldCut]) -> String {
    let mut text =
        String::from("ply\tside\tkey\twhich\tmove\tdepth\tcut_ply\tcut_key\tstop\tmoves\n");
    for c in cuts {
        let moves: Vec<&str> = std::iter::once(c.mv.as_str())
            .chain(c.cut.moves.iter().map(String::as_str))
            .collect();
        text.push_str(
            &[
                c.ply.to_string(),
                c.side().into(),
                c.key.into(),
                c.which.into(),
                c.mv.clone(),
                c.depth.to_string(),
                c.cut_ply().to_string(),
                c.cut.key.clone(),
                c.cut.stop.as_str().into(),
                moves.join(" "),
            ]
            .join("\t"),
        );
        text.push('\n');
    }
    text
}

/// 途切れた局面と保留の局面の手番の側の組ごとに 1 行にする。根の手数は最も小さい手数にし、
/// 保留の局面の手数・途切れの手数・局面の順に priority の順位を付ける。返す数は既存の木で除いた組の数。
fn cuts_queue(cuts: &[HeldCut], args: &HoldEarlyArgs) -> Result<(Vec<Line>, usize)> {
    let excluded = excluded_positions(&args.cuts_exclude_queue)?;
    let mut groups: BTreeMap<(&str, &str), Vec<&HeldCut>> = BTreeMap::new();
    for c in cuts {
        groups.entry((c.cut.key.as_str(), c.side())).or_default().push(c);
    }
    let total = groups.len();
    let mut picked: Vec<_> = groups
        .into_iter()
        .filter(|((key, own), _)| !excluded.contains_key(&((*key).to_owned(), (*own).to_owned())))
        .map(|((key, own), from)| {
            let hold_ply = from.iter().map(|c| c.ply).min().unwrap_or_default();
            let root_ply = from.iter().map(|c| c.cut_ply()).min().unwrap_or_default();
            (hold_ply, root_ply, key, own, from)
        })
        .collect();
    let excluded = total - picked.len();
    picked.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    let mut lines = Vec::new();
    for (rank, (hold_ply, root_ply, key, own, from)) in picked.into_iter().enumerate() {
        let sources: Vec<String> = from
            .iter()
            .map(|c| format!("ply={} key={} {}={} depth={}", c.ply, c.key, c.which, c.mv, c.depth))
            .collect();
        let line = Line {
            group: None,
            id: queue_id(&args.cuts_tag, usize::try_from(root_ply)?, key, own),
            root: format!("sfen {key} {root_ply}"),
            roots: Vec::new(),
            own: own.to_owned(),
            depth: args.cuts_depth,
            priority: args.cuts_priority + rank.min(99) as u32,
            hold: false,
            game: Vec::new(),
            opts: args.cuts_opts(),
            memo: Some(format!("hold-early hold_ply={hold_ply} {}", sources.join("; "))),
        };
        line.check().with_context(|| format!("キューの行の検査: {key}"))?;
        lines.push(line);
    }
    Ok((lines, excluded))
}

pub(super) fn run(args: HoldEarlyArgs) -> Result<()> {
    run_to(args, &mut std::io::stdout().lock())
}

fn value_black(row: &Row) -> Result<String> {
    Ok(mean_eval(row.get("evals"))?
        .map(|value| {
            let value = i64::from(value);
            if row.get("key").split_whitespace().nth(1) == Some("w") {
                -value
            } else {
                value
            }
            .to_string()
        })
        .unwrap_or_default())
}

fn continuation_depth(
    key: &str,
    mv: &str,
    rows: &[Row],
    mains: &BTreeMap<String, usize>,
) -> Result<(usize, Option<Cut>)> {
    let mut position = position_from_sfen(&format!("{key} 1"))?;
    apply_usi_move(&mut position, mv)?;
    let own = key.split_whitespace().nth(1);
    // 循環は定跡が先へ伸びていないので、`master audit --deep-plies` と同じく同じ局面に戻ったところで止める。
    let mut seen = BTreeSet::from([key.to_owned()]);
    let mut moves = Vec::new();
    for depth in 0..60 {
        let sfen = position.to_sfen();
        let child = strip_ply(&sfen);
        let cut = |stop| Cut {
            key: child.to_owned(),
            stop,
            moves: moves.clone(),
        };
        let Some(&index) = mains.get(child) else {
            let stop = if child.split_whitespace().nth(1) == own {
                Stop::Own
            } else {
                Stop::Opp
            };
            return Ok((depth, Some(cut(stop))));
        };
        if !seen.insert(child.to_owned()) {
            return Ok((depth, Some(cut(Stop::Cycle))));
        }
        let mv = rows[index].get("move");
        apply_usi_move(&mut position, mv)?;
        moves.push(mv.to_owned());
    }
    Ok((60, None))
}

fn run_to(args: HoldEarlyArgs, stdout: &mut impl Write) -> Result<()> {
    ensure!(
        !args.since.trim().is_empty() && !args.since.contains(['\t', '\r', '\n']),
        "since は空でない 1 セルのタグにしてください"
    );
    let mut protected = vec![args.before.clone()];
    if let Some(path) = &args.hold_keys {
        protected.push(path.clone());
    }
    if let Some(path) = &args.deep.deep_opp_freq {
        protected.push(path.clone());
    }
    if let Some(dir) = &args.schema {
        protected.extend(schema::paths(dir));
    }
    protected.extend(args.cuts_exclude_queue.iter().cloned());
    files::guard(std::slice::from_ref(&args.master), &protected, true)?;
    protected.push(args.master.clone());
    let outputs: Vec<PathBuf> = [
        Some(&args.out),
        args.cuts_out.as_ref(),
        args.cuts_enqueue_out.as_ref(),
    ]
    .into_iter()
    .flatten()
    .cloned()
    .collect();
    let guard_outputs = || -> Result<()> {
        for (i, output) in outputs.iter().enumerate() {
            let mut inputs = protected.clone();
            inputs.extend(outputs[..i].iter().cloned());
            files::guard(std::slice::from_ref(output), &inputs, true)?;
        }
        Ok(())
    };
    guard_outputs()?;
    check_tag(&args.cuts_tag).context("--cuts-tag")?;
    args.cuts_opts().check()?;
    let schema = Schema::load(args.schema.as_deref())?;
    let freq = args
        .deep
        .deep_opp_freq
        .as_deref()
        .map(|path| ReplyFrequency::read(path, args.deep.deep_min_games))
        .transpose()?;
    let mut hold_keys = BTreeSet::new();
    if let Some(path) = &args.hold_keys {
        for line in BufReader::new(File::open(path)?).lines() {
            let line = line?;
            let key = line.trim();
            if !key.is_empty() && !key.starts_with('#') {
                hold_keys.insert(key.to_owned());
            }
        }
    }
    let before = std::fs::read_to_string(&args.before)?;
    let original = std::fs::read_to_string(&args.master)?;
    for (path, text) in [(&args.before, &before), (&args.master, &original)] {
        let errors = schema.validate(text)?;
        ensure!(errors.is_empty(), "{}: {}", path.display(), errors.join("\n"));
    }
    let before = parse_master(&before)?;
    let mut rows = parse_master(&original)?;
    let mains: BTreeMap<_, _> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.get("role") == "main")
        .map(|(i, r)| (r.get("key").to_owned(), i))
        .collect();
    let indices: BTreeMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| ((r.get("key").to_owned(), r.get("move").to_owned()), i))
        .collect();
    let mut changes = Vec::new();
    for old in before.iter().filter(|r| r.get("role") == "main") {
        let Ok(ply) = old.get("ply").parse::<u64>() else {
            continue;
        };
        if ply >= args.below_ply {
            continue;
        }
        if let Some(&index) = mains.get(old.get("key"))
            && rows[index].get("move") != old.get("move")
        {
            let depth = |mv: &str| {
                if let Some(freq) = &freq {
                    Continuation {
                        freq,
                        main: |key: &str| mains.get(key).map(|&i| rows[i].get("move").to_owned()),
                    }
                    .after_with_cut(
                        old.get("key"),
                        mv,
                        args.continuation.unwrap_or(60),
                    )
                } else {
                    continuation_depth(old.get("key"), mv, &rows, &mains)
                }
            };
            let (old_depth, old_cut) = depth(old.get("move"))?;
            let (new_depth, new_cut) = depth(rows[index].get("move"))?;
            let replace = !hold_keys.contains(old.get("key"))
                && args.continuation.is_some_and(|n| old_depth >= n && new_depth >= n);
            changes.push((ply, old, index, [(old_depth, old_cut), (new_depth, new_cut)], replace));
        }
    }
    changes.sort_by(|a, b| (a.0, a.1.get("key")).cmp(&(b.0, b.1.get("key"))));
    let mut table = String::from(
        "ply\tside\tkey\told_main\told_source\told_tier\told_value_black\tnew_main\tnew_value_black\told_depth\tnew_depth\tclass\n",
    );
    let mut cuts = Vec::new();
    for (ply, old, index, depths, replace) in &changes {
        let Some(n) = args.continuation.filter(|_| !*replace) else {
            continue;
        };
        for ((depth, cut), (which, mv)) in
            depths.iter().zip([("old", old.get("move")), ("new", rows[*index].get("move"))])
        {
            if let Some(cut) = cut.as_ref().filter(|_| *depth < n) {
                cuts.push(HeldCut {
                    ply: *ply,
                    key: old.get("key"),
                    which,
                    mv: mv.to_owned(),
                    depth: *depth,
                    cut,
                });
            }
        }
    }
    let cuts_table = args.cuts_out.as_ref().map(|_| format_cuts(&cuts));
    let queue = match &args.cuts_enqueue_out {
        Some(_) => {
            let (lines, excluded) = cuts_queue(&cuts, &args)?;
            Some((format_queue(&lines)?, lines.len(), excluded))
        }
        None => None,
    };
    let cut_positions = cuts.iter().map(|c| (&c.cut.key, c.side())).collect::<BTreeSet<_>>().len();
    let cut_count = cuts.len();
    let mut counts = [0usize; 2];
    let mut held = 0;
    for (ply, old, index, [(old_depth, _), (new_depth, _)], replace) in &changes {
        let key = old.get("key");
        let side = key.split_whitespace().nth(1).unwrap_or("");
        counts[usize::from(side == "w")] += 1;
        let new = &rows[*index];
        table.push_str(
            &[
                ply.to_string(),
                side.into(),
                key.into(),
                old.get("move").into(),
                old.get("source").into(),
                old.get("tier").into(),
                value_black(old)?,
                new.get("move").into(),
                value_black(new)?,
                old_depth.to_string(),
                new_depth.to_string(),
                if *replace { "replace" } else { "hold" }.into(),
            ]
            .join("\t"),
        );
        table.push('\n');
        if *replace {
            continue;
        }
        held += 1;
        let restored = match indices.get(&(key.to_owned(), old.get("move").to_owned())) {
            Some(&i) => i,
            None => {
                rows.push((*old).clone());
                rows.len() - 1
            }
        };
        rows[restored].set("role", "main".into());
        rows[restored].set("tier", old.get("tier").into());
        if old.get("tier") == "curated" {
            rows[*index].set("tier", "wide".into());
        }
        rows[*index].set("role", "alt".into());
        for i in [restored, *index] {
            let note = rows[i].get("note").to_owned();
            rows[i].set(
                "note",
                format!(
                    "{note}{}{}: {} 手目より前、読みが浅いので保留 (今の main)",
                    if note.is_empty() { "" } else { "; " },
                    args.since,
                    args.below_ply
                ),
            );
        }
    }
    let updated = format_master(&rows);
    let errors = schema.validate(&updated)?;
    ensure!(errors.is_empty(), "{}", errors.join("\n"));
    guard_outputs()?;
    write_atomic(&args.out, &table)?;
    if let (Some(path), Some(text)) = (&args.cuts_out, &cuts_table) {
        write_atomic(path, text)?;
    }
    if let (Some(path), Some((text, _, _))) = (&args.cuts_enqueue_out, &queue) {
        write_atomic(path, text)?;
    }
    if args.apply && held > 0 {
        files::guard(std::slice::from_ref(&args.master), &protected[..protected.len() - 1], true)?;
        ensure!(
            std::fs::read_to_string(&args.master)? == original,
            "元データが点検中に変更されたため保存しません"
        );
        write_atomic(&args.master, &updated)?;
    }
    writeln!(
        stdout,
        "対象局面: {} (先手番: {}, 後手番: {})",
        changes.len(),
        counts[0],
        counts[1]
    )?;
    if let Some(path) = &args.deep.deep_opp_freq {
        writeln!(stdout, "継続判定の相手頻度定跡: {}", path.display())?;
    }
    writeln!(stdout, "分類: hold {held}, replace {}", changes.len() - held)?;
    writeln!(stdout, "書き戻した局面: {}", if args.apply { held } else { 0 })?;
    if args.cuts_out.is_some() || args.cuts_enqueue_out.is_some() {
        writeln!(stdout, "継続が足りない枝: {cut_count} (途切れた局面と側: {cut_positions})")?;
    }
    if let Some((_, queued, excluded)) = &queue {
        writeln!(stdout, "途切れた局面のキューの行: {queued} (既存の木で除外 {excluded})")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
