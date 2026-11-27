//! 元データへの行の追加と置換。
use super::{
    export::{self, Schema, files},
    master_table::{self, COLUMNS, ROLES, Row},
    model::side,
};
use crate::book_db::{apply_usi_move, position_from_sfen, strip_ply};
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use rshogi_core::{position::SFEN_HIRATE, types::Move};
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
    /// 追加・置換する行の表 (同じ 13 列)
    #[arg(long, required_unless_present = "line")]
    add: Option<PathBuf>,
    /// 固定の行を作る平手からの USI 手順
    #[arg(long, conflicts_with_all = ["add", "replace"], requires_all = ["from", "to", "source", "since", "note", "schema"])]
    line: Option<String>,
    /// 追加区間の最初の手数 (初手は 1)
    #[arg(long, requires = "line")]
    from: Option<usize>,
    /// 追加区間の最後の手数 (この手を含む)
    #[arg(long, requires = "line")]
    to: Option<usize>,
    /// 固定行の source (sources.tsv の語彙)
    #[arg(long, requires = "line")]
    source: Option<String>,
    /// 固定行の since
    #[arg(long, requires = "line")]
    since: Option<String>,
    /// 固定行の note
    #[arg(long, requires = "line")]
    note: Option<String>,
    /// 検査後に元データを書き換える
    #[arg(long)]
    apply: bool,
    /// 同じ局面と手の既存行を全列置き換える
    #[arg(long)]
    replace: bool,
    /// source・読み条件・lineage の種類の表を置いたディレクトリ
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
    if let Err(error) = master_table::check_header(lines.next().unwrap_or("")) {
        errors.push(format!("{}: {error}", path.display()));
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

fn line_rows(line: &str, args: &AddArgs, schema: &Schema) -> Result<Vec<Row>> {
    let tokens: Vec<_> = line.split_whitespace().collect();
    let moves = match tokens.as_slice() {
        ["position", "startpos", "moves", moves @ ..] | ["startpos", "moves", moves @ ..] => moves,
        _ => bail!("--line は position startpos moves または startpos moves で始めてください"),
    };
    let from = args.from.context("--from が必要です")?;
    let to = args.to.context("--to が必要です")?;
    ensure!(
        1 <= from && from <= to && to <= moves.len(),
        "手数の範囲 {from}〜{to} が手順 (全 {} 手) の外です",
        moves.len()
    );
    let source = args.source.as_deref().context("--source が必要です")?;
    ensure!(
        schema.sources.iter().any(|(name, _)| name == source),
        "unknown source: {source}"
    );
    let since = args.since.as_deref().context("--since が必要です")?;
    let note = args.note.as_deref().context("--note が必要です")?;
    for (name, value) in [("since", since), ("note", note)] {
        ensure!(
            !value.is_empty() && !value.contains(['\t', '\r', '\n']),
            "--{name} が空か、タブ・改行を含みます"
        );
    }
    ensure!(!since.trim().is_empty(), "--since が空です");

    let mut pos = position_from_sfen(SFEN_HIRATE)?;
    let mut rows = Vec::new();
    for (i, &mv) in moves.iter().enumerate() {
        let ply = i + 1;
        ensure!(
            Move::from_usi_strict(mv).is_some_and(Move::is_normal),
            "{ply} 手目 {mv}: USI 指し手が不正です"
        );
        if (from..=to).contains(&ply) {
            rows.push(Row::from_fields(&[
                ("key", strip_ply(&pos.to_sfen()).to_owned()),
                ("ply", ply.to_string()),
                ("move", mv.to_owned()),
                ("role", "main".into()),
                ("source", source.into()),
                ("tier", "wide".into()),
                ("since", since.into()),
                ("note", note.into()),
            ]));
        }
        apply_usi_move(&mut pos, mv).with_context(|| format!("{ply} 手目 {mv}"))?;
    }
    Ok(rows)
}

fn line_additions(candidates: &[Row], rows: &[Row], out: &mut impl Write) -> Result<Vec<Row>> {
    let mut positions: BTreeMap<&str, Vec<&Row>> =
        candidates.iter().map(|row| (row.get("key"), Vec::new())).collect();
    for row in rows {
        if let Some(existing) = positions.get_mut(row.get("key")) {
            existing.push(row);
        }
    }
    let mut fresh = Vec::new();
    let mut skipped = 0;
    let mut blocked = Vec::new();
    for row in candidates {
        let key = row.get("key");
        let existing = positions.entry(key).or_default();
        let ply = row.get("ply");
        let mv = row.get("move");
        let turn = if side(key) == "b" { "先手" } else { "後手" };
        match existing.as_slice() {
            [] => {
                writeln!(out, "add: {ply} 手目 {turn} {mv}")?;
                existing.push(row);
                fresh.push(row.clone());
            }
            [old] if old.get("move") == mv && old.get("role") == "main" => {
                writeln!(out, "skip: {ply} 手目 {turn} {mv}")?;
                skipped += 1;
            }
            _ => {
                writeln!(out, "blocked: {ply} 手目 {turn} {mv}")?;
                let details: Vec<_> = existing
                    .iter()
                    .map(|r| {
                        format!(
                            "{} role={} evals={:?}",
                            r.get("move"),
                            r.get("role"),
                            r.get("evals")
                        )
                    })
                    .collect();
                blocked.push(format!("{ply} 手目 {turn} {mv}: {}", details.join(", ")));
            }
        }
    }
    writeln!(out, "line: add={}, skip={skipped}, blocked={}", fresh.len(), blocked.len())?;
    ensure!(blocked.is_empty(), "固定行が衝突しました:\n{}", blocked.join("\n"));
    Ok(fresh)
}

fn run_to(args: AddArgs, out: &mut impl Write) -> Result<()> {
    let mut protected: Vec<_> = args.add.iter().cloned().collect();
    if let Some(dir) = &args.schema {
        protected.extend(super::schema::paths(dir));
    }
    files::guard(std::slice::from_ref(&args.master), &protected, true)?;
    let schema = Schema::load(args.schema.as_deref())?;
    let (mut rows, mut errors) = read_table(&args.master)?;
    let (added, input_errors) = if let Some(line) = &args.line {
        (line_additions(&line_rows(line, &args, &schema)?, &rows, out)?, Vec::new())
    } else {
        read_table(args.add.as_deref().context("--add が必要です")?)?
    };
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
    let mut changed = [0usize; COLUMNS.len()];
    let mut replacements = Vec::new();
    for row in &added {
        if let Err(error) = schema.validate_row(row) {
            errors.push(error.to_string());
        }
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
                    if !differences.is_empty() {
                        replacements.push(format!(
                            "replaced: {} | {}: {}",
                            pair.0,
                            pair.1,
                            differences.join(", ")
                        ));
                    }
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
    if args.line.is_none() {
        writeln!(out, "skipped existing rows: {}", added.len() - fresh.len() - replaced)?;
    }
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
    if args.apply && args.line.is_some() && fresh.is_empty() {
        writeln!(out, "applied: rows {original_len} -> {original_len} (no changes)")?;
    } else if args.apply {
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
