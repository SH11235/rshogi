//! 序盤の既存局面の main を戻し、保留する変更を一覧にする。
use super::{
    export::files,
    master_table::{Row, format_master, mean_eval, parse_master},
    schema::{self, Schema},
};
use crate::common::io::write_atomic;
use anyhow::{Result, ensure};
use clap::Args;
use std::{collections::BTreeMap, io::Write, path::PathBuf};

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

fn run_to(args: HoldEarlyArgs, stdout: &mut impl Write) -> Result<()> {
    ensure!(
        !args.since.trim().is_empty() && !args.since.contains(['\t', '\r', '\n']),
        "since は空でない 1 セルのタグにしてください"
    );
    let mut protected = vec![args.before.clone()];
    if let Some(dir) = &args.schema {
        protected.extend(schema::paths(dir));
    }
    files::guard(std::slice::from_ref(&args.master), &protected, true)?;
    protected.push(args.master.clone());
    files::guard(std::slice::from_ref(&args.out), &protected, true)?;
    let schema = Schema::load(args.schema.as_deref())?;
    let before = std::fs::read_to_string(&args.before)?;
    let original = std::fs::read_to_string(&args.master)?;
    for text in [&before, &original] {
        let errors = schema.validate(text)?;
        ensure!(errors.is_empty(), "{}", errors.join("\n"));
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
            changes.push((ply, old, index));
        }
    }
    changes.sort_by(|a, b| (a.0, a.1.get("key")).cmp(&(b.0, b.1.get("key"))));
    let mut table = String::from(
        "ply\tside\tkey\told_main\told_source\told_tier\told_value_black\tnew_main\tnew_value_black\n",
    );
    let mut counts = [0usize; 2];
    for (ply, old, index) in &changes {
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
            ]
            .join("\t"),
        );
        table.push('\n');
        let restored = match indices.get(&(key.to_owned(), old.get("move").to_owned())) {
            Some(&i) => i,
            None => {
                rows.push((*old).clone());
                rows.len() - 1
            }
        };
        rows[restored].set("role", "main".into());
        if old.get("tier") == "curated" {
            rows[restored].set("tier", "curated".into());
            rows[*index].set("tier", "wide".into());
        }
        rows[*index].set("role", "alt".into());
        for i in [restored, *index] {
            let note = rows[i].get("note").to_owned();
            rows[i].set(
                "note",
                format!(
                    "{note}{}{}: {} 手目より前の main の替わりを保留に戻した",
                    if note.is_empty() { "" } else { "。" },
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
    if args.apply && !changes.is_empty() {
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
    writeln!(stdout, "書き戻した局面: {}", if args.apply { changes.len() } else { 0 })?;
    Ok(())
}

#[cfg(test)]
mod tests;
