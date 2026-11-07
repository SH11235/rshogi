//! 序盤の既存局面の main を戻し、保留する変更を一覧にする。
use super::{
    deep::{Continuation, DeepArgs, ReplyFrequency},
    export::files,
    master_table::{Row, format_master, mean_eval, parse_master},
    schema::{self, Schema},
};
use crate::book_db::{apply_usi_move, position_from_sfen, strip_ply};
use crate::common::io::write_atomic;
use anyhow::{Result, ensure};
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
) -> Result<usize> {
    let mut position = position_from_sfen(&format!("{key} 1"))?;
    apply_usi_move(&mut position, mv)?;
    // 循環は定跡が先へ伸びていないので、`master audit --deep-plies` と同じく同じ局面に戻ったところで止める。
    let mut seen = BTreeSet::from([key.to_owned()]);
    for depth in 0..60 {
        let sfen = position.to_sfen();
        let child = strip_ply(&sfen);
        let Some(&index) = mains.get(child) else {
            return Ok(depth);
        };
        if !seen.insert(child.to_owned()) {
            return Ok(depth);
        }
        apply_usi_move(&mut position, rows[index].get("move"))?;
    }
    Ok(60)
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
    files::guard(std::slice::from_ref(&args.master), &protected, true)?;
    protected.push(args.master.clone());
    files::guard(std::slice::from_ref(&args.out), &protected, true)?;
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
                    .after(old.get("key"), mv, args.continuation.unwrap_or(60))
                } else {
                    continuation_depth(old.get("key"), mv, &rows, &mains)
                }
            };
            let old_depth = depth(old.get("move"))?;
            let new_depth = depth(rows[index].get("move"))?;
            let replace = !hold_keys.contains(old.get("key"))
                && args.continuation.is_some_and(|n| old_depth >= n && new_depth >= n);
            changes.push((ply, old, index, old_depth, new_depth, replace));
        }
    }
    changes.sort_by(|a, b| (a.0, a.1.get("key")).cmp(&(b.0, b.1.get("key"))));
    let mut table = String::from(
        "ply\tside\tkey\told_main\told_source\told_tier\told_value_black\tnew_main\tnew_value_black\told_depth\tnew_depth\tclass\n",
    );
    let mut counts = [0usize; 2];
    let mut held = 0;
    for (ply, old, index, old_depth, new_depth, replace) in &changes {
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
    files::guard(std::slice::from_ref(&args.out), &protected, true)?;
    write_atomic(&args.out, &table)?;
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
    Ok(())
}

#[cfg(test)]
mod tests;
