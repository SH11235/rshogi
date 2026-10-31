//! 元データへの行の追加と置換。
use super::{
    export::{self, Schema, files},
    master_table::{self, COLUMNS, ROLES, Row},
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Args, Debug)]
pub(super) struct AddArgs {
    /// 元データの表
    #[arg(long)]
    master: PathBuf,
    /// 追加・置換する行の表 (同じ 11 列)
    #[arg(long)]
    add: PathBuf,
    /// 検査後に元データを書き換える
    #[arg(long)]
    apply: bool,
    /// 同じ局面と手の既存行を全列置き換える
    #[arg(long)]
    replace: bool,
    /// sources.tsv を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
}

pub(super) fn sort_rows(rows: &mut [Row]) {
    rows.sort_by(|a, b| {
        let order = |r: &Row| ROLES.iter().position(|s| *s == r.get("role")).unwrap_or(ROLES.len());
        (a.get("key"), order(a), a.get("move")).cmp(&(b.get("key"), order(b), b.get("move")))
    });
}

pub(super) fn read_table(path: &Path) -> Result<(Vec<Row>, Vec<String>)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("読めません: {}", path.display()))?;
    let mut lines = text.lines();
    let mut errors = Vec::new();
    if lines.next() != Some(COLUMNS.join("\t").as_str()) {
        errors.push(format!("{}: 元データの見出しが違います", path.display()));
    }
    let mut rows = Vec::new();
    for (i, line) in lines.enumerate() {
        match Row::parse_line(line, i + 2) {
            Ok(row) => rows.push(row),
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }
    Ok((rows, errors))
}

pub(super) fn run(args: AddArgs) -> Result<()> {
    run_to(args, &mut std::io::stdout().lock())
}
fn run_to(args: AddArgs, out: &mut impl Write) -> Result<()> {
    let mut protected = vec![args.add.clone()];
    if let Some(dir) = &args.schema {
        protected.push(dir.join("sources.tsv"));
    }
    files::guard(std::slice::from_ref(&args.master), &protected, true)?;
    let schema = Schema::load(args.schema.as_deref())?;
    let (mut rows, mut errors) = read_table(&args.master)?;
    let (added, input_errors) = read_table(&args.add)?;
    errors.extend(input_errors);
    errors.extend(schema.validate(master_table::format_master(&rows))?);
    let original_len = rows.len();
    let original_keys: BTreeSet<_> = rows.iter().map(|r| r.get("key").to_owned()).collect();
    let originals: BTreeMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| ((r.get("key").to_owned(), r.get("move").to_owned()), i))
        .collect();
    let mut seen = BTreeSet::new();
    let mut fresh = Vec::new();
    let mut replaced = 0;
    let mut changed = [0usize; 11];
    let mut replacements = Vec::new();
    for row in &added {
        let pair = (row.get("key").to_owned(), row.get("move").to_owned());
        if !seen.insert(pair.clone()) {
            errors.push(format!("追加する表で同じ局面と手が重なっています: {} {}", pair.0, pair.1));
        }
        if let Some(&index) = originals.get(&pair) {
            let old = &rows[index];
            if args.replace {
                replaced += 1;
                for (i, field) in COLUMNS.iter().enumerate() {
                    changed[i] += usize::from(old.get(field) != row.get(field));
                }
                if old.get("tier") != row.get("tier") || old.get("tier") == "curated" {
                    let differences: Vec<_> = COLUMNS
                        .iter()
                        .filter(|f| old.get(f) != row.get(f))
                        .map(|f| format!("{f}: {} -> {}", old.get(f), row.get(f)))
                        .collect();
                    replacements.push(format!(
                        "replaced: {} | {}: {}",
                        pair.0,
                        pair.1,
                        differences.join(", ")
                    ));
                }
                rows[index] = row.clone();
            } else {
                let differences: Vec<_> = ["role", "tier", "source"]
                    .iter()
                    .filter(|f| old.get(f) != row.get(f))
                    .map(|f| format!("{f}: {} -> {}", old.get(f), row.get(f)))
                    .collect();
                if !differences.is_empty() {
                    writeln!(
                        out,
                        "skipped conflict: {} | {}: {}",
                        pair.0,
                        pair.1,
                        differences.join(", ")
                    )?;
                }
            }
        } else {
            fresh.push(row.clone());
        }
    }
    rows.extend(fresh.iter().cloned());
    errors.extend(schema.validate(master_table::format_master(&rows))?);
    ensure!(errors.is_empty(), "{}", errors.join("\n"));
    sort_rows(&mut rows);
    for replacement in replacements {
        writeln!(out, "{replacement}")?;
    }
    writeln!(out, "skipped existing rows: {}", added.len() - fresh.len() - replaced)?;
    if args.replace {
        writeln!(out, "replaced existing rows: {replaced}")?;
        for (field, count) in COLUMNS.iter().zip(changed) {
            if count > 0 {
                writeln!(out, "changed {field}: {count}")?;
            }
        }
    }
    let new_positions = fresh
        .iter()
        .map(|r| r.get("key"))
        .filter(|k| !original_keys.contains(*k))
        .collect::<BTreeSet<_>>()
        .len();
    writeln!(out, "added positions: {new_positions}; added moves: {}", fresh.len())?;
    export::print_counts(&fresh, &schema, out)?;
    for tier in ["curated", "wide"] {
        for role in ROLES {
            writeln!(
                out,
                "added {tier}/{role}: {}",
                fresh.iter().filter(|r| r.get("tier") == tier && r.get("role") == role).count()
            )?;
        }
    }
    if args.apply {
        let parent = args
            .master
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(master_table::format_master(&rows).as_bytes())?;
        temporary.flush()?;
        files::guard(std::slice::from_ref(&args.master), &protected, true)?;
        temporary.persist(&args.master).map_err(|e| e.error)?;
        writeln!(out, "applied: rows {original_len} -> {}", rows.len())?;
    } else {
        writeln!(out, "OK (dry run): rows {original_len} -> {}; no files written", rows.len())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
