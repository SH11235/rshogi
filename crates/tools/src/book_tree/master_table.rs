//! 定跡の元データの列、行、読み書きと検査。

use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::Path;

pub(super) const COLUMNS: [&str; 11] = [
    "key", "ply", "move", "role", "source", "evals", "nodes", "lineage", "tier", "since", "note",
];
pub(super) const ROLES: [&str; 3] = ["main", "alt", "record"];
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Row(Vec<String>);
impl Row {
    pub(super) fn from_fields(fields: &[(&str, String)]) -> Self {
        let mut row = Self(vec![String::new(); COLUMNS.len()]);
        for (name, value) in fields {
            row.set(name, value.clone());
        }
        row
    }
    pub(super) fn parse_line(line: &str, at: usize) -> Result<Self> {
        let row = Self::split_line(line);
        ensure!(row.0.len() == COLUMNS.len(), "元データの {at} 行目の列の数が違います");
        Ok(row)
    }
    // 事前検査済みの表から一部の列だけ再読する経路では、列数や role を再検査しない。
    pub(super) fn split_line(line: &str) -> Self {
        Self(line.split('\t').map(str::to_owned).collect())
    }
    pub(super) fn get_optional(&self, column: &str) -> Option<&str> {
        self.0.get(index(column)).map(String::as_str)
    }
    pub(super) fn get(&self, column: &str) -> &str {
        &self.0[index(column)]
    }
    pub(super) fn set(&mut self, column: &str, value: String) {
        self.0[index(column)] = value;
    }
}
fn index(column: &str) -> usize {
    COLUMNS.iter().position(|c| *c == column).expect("既知の列")
}

/// 局面の main・alt の手。record は指さないので持たない。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct BookMove {
    pub mv: String,
    /// 読みの値の平均を丸めた値 (手番の側から)。読みが無ければ `None`。
    pub eval: Option<i32>,
}

/// 元データの局面。`moves[0]` が main。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct MasterPosition {
    pub ply: String,
    pub source: String,
    pub moves: Vec<BookMove>,
}

pub(super) fn parse_master(text: &str) -> Result<Vec<Row>> {
    read_rows(text.as_bytes())?.map(|row| row.map(|(_, row)| row)).collect()
}

pub(super) fn read_rows(
    reader: impl BufRead,
) -> Result<impl Iterator<Item = Result<(usize, Row)>>> {
    let mut lines = reader.lines();
    let header = lines.next().transpose()?;
    ensure!(
        header.as_deref() == Some(COLUMNS.join("\t").as_str()),
        "元データの見出しが違います"
    );
    Ok(lines.enumerate().map(|(i, line)| {
        let at = i + 2;
        let row = Row::parse_line(&line?, at)?;
        ensure!(ROLES.contains(&row.get("role")), "元データの {at} 行目の role が不正です");
        Ok((at, row))
    }))
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

/// 局面ごとに main・alt の手を main を先頭に集め、record は数だけ返す。
pub(super) fn read_master(path: &Path) -> Result<(BTreeMap<String, MasterPosition>, usize)> {
    let file =
        std::fs::File::open(path).with_context(|| format!("{} を読めません", path.display()))?;
    parse_master_rows(BufReader::new(file))
}

pub(super) fn parse_master_rows(
    reader: impl BufRead,
) -> Result<(BTreeMap<String, MasterPosition>, usize)> {
    positions_from_rows(read_rows(reader)?)
}

pub(super) fn positions_from_rows(
    rows: impl IntoIterator<Item = Result<(usize, Row)>>,
) -> Result<(BTreeMap<String, MasterPosition>, usize)> {
    let mut positions: BTreeMap<String, MasterPosition> = BTreeMap::new();
    let mut records = 0;
    for row in rows {
        let (at, row) = row?;
        let role = row.get("role");
        if role == "record" {
            records += 1;
            continue;
        }
        let eval = mean_eval(row.get("evals"))
            .with_context(|| format!("元データの {at} 行目の evals が不正です"))?;
        let position =
            positions.entry(row.get("key").to_owned()).or_insert_with(|| MasterPosition {
                ply: String::new(),
                source: String::new(),
                moves: Vec::new(),
            });
        ensure!(
            position.moves.iter().all(|m| m.mv != row.get("move")),
            "元データの {at} 行目の手が同じ局面で重なっています"
        );
        let book_move = BookMove {
            mv: row.get("move").to_owned(),
            eval,
        };
        if role == "main" {
            ensure!(position.ply.is_empty(), "元データの {at} 行目: 局面に main が 2 つあります");
            position.ply = row.get("ply").to_owned();
            position.source = row.get("source").to_owned();
            position.moves.insert(0, book_move);
        } else {
            position.moves.push(book_move);
        }
    }
    if let Some((key, _)) = positions.iter().find(|(_, p)| p.ply.is_empty()) {
        bail!("main の無い局面があります: {key}");
    }
    Ok((positions, records))
}

/// カンマ区切りの読みの値の平均を、0.5 は偶数の側へ丸める (偶数丸め)。空なら `None`。
pub(super) fn mean_eval(text: &str) -> Result<Option<i32>> {
    if text.is_empty() {
        return Ok(None);
    }
    let values: Vec<i64> = text.split(',').map(str::parse).collect::<Result<_, _>>()?;
    let sum = values.iter().try_fold(0_i64, |sum, value| {
        sum.checked_add(*value).context("evals の合計が i64 の範囲を超えています")
    })?;
    let mean = sum as f64 / values.len() as f64;
    Ok(Some(i32::try_from(mean.round_ties_even() as i64)?))
}

/// 局面ごとに main がちょうど 1 つ (丁寧な定跡の行だけでも 1 つ) で、同じ手の行が重ならないこと。
pub(super) fn validate(rows: &[Row]) -> Result<()> {
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

#[derive(clap::Args, Debug)]
pub(super) struct ValidateArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: std::path::PathBuf,
}

pub(super) fn run_validate(args: ValidateArgs) -> Result<()> {
    let file = std::fs::File::open(&args.master)
        .with_context(|| format!("{} を読めません", args.master.display()))?;
    let report = validate_table(BufReader::new(file))?;
    write_validation_report(&report, std::io::stdout().lock())
}

fn write_validation_report(
    report: &ValidationReport,
    mut stdout: impl std::io::Write,
) -> Result<()> {
    for error in &report.errors {
        eprintln!("{error}");
    }
    ensure!(
        report.errors.is_empty(),
        "元データに {} 件の誤りがあります",
        report.errors.len()
    );
    write!(stdout, "{}", report.summary)?;
    Ok(())
}

#[derive(Default, Debug)]
pub(super) struct ValidationReport {
    pub(super) errors: Vec<String>,
    summary: String,
}

#[derive(Default)]
struct PositionRows {
    moves: BTreeMap<String, Vec<usize>>,
    first: usize,
    playable: [usize; 2],
    mains: [usize; 2],
}

fn without_line_ending(line: &str) -> &str {
    match line.strip_suffix('\n') {
        Some(line) => line.strip_suffix('\r').unwrap_or(line),
        None => line,
    }
}

pub(super) fn validate_table(mut reader: impl BufRead) -> Result<ValidationReport> {
    use crate::book_db::{position_from_sfen, strip_ply};
    use rshogi_core::movegen::{MoveList, generate_legal_all};
    use std::fmt::Write as _;

    let mut report = ValidationReport::default();
    let mut errors = Vec::new();
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    match std::str::from_utf8(&line) {
        Err(_) => errors.push((1, "元データの 1 行目: UTF-8 ではありません".into())),
        Ok(line) if without_line_ending(line) != COLUMNS.join("\t") => {
            errors.push((1, "元データの 1 行目の見出しが違います".into()));
        }
        Ok(_) => {}
    }
    let mut groups: BTreeMap<String, PositionRows> = BTreeMap::new();
    let mut tiers: BTreeMap<String, (BTreeSet<String>, usize)> = ["curated", "wide"]
        .into_iter()
        .map(|tier| (tier.to_owned(), (BTreeSet::new(), 0)))
        .collect();
    let mut roles: BTreeMap<String, usize> =
        ROLES.into_iter().map(|role| (role.to_owned(), 0)).collect();
    let mut sources: BTreeMap<String, usize> = BTreeMap::new();
    let mut count = 0;
    let mut at = 1;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        at += 1;
        let text = match std::str::from_utf8(&line) {
            Ok(text) => text,
            Err(_) => {
                errors.push((at, format!("元データの {at} 行目: UTF-8 ではありません")));
                continue;
            }
        };
        let row = match Row::parse_line(without_line_ending(text), at) {
            Ok(row) => row,
            Err(error) => {
                errors.push((at, error.to_string()));
                continue;
            }
        };
        count += 1;
        let mut error =
            |message: &str| errors.push((at, format!("元データの {at} 行目: {message}")));
        if row.0.iter().any(|cell| cell.contains('\r')) {
            error("セルに CR は使えません");
        }
        if row.get("since").trim().is_empty() {
            error("since が空です");
        }
        let role = row.get("role");
        if !ROLES.contains(&role) {
            error("role が不正です (main/alt/record)");
        }
        let tier = row.get("tier");
        if !tiers.contains_key(tier) {
            error("tier が不正です (curated/wide)");
        }
        if !row.get("ply").bytes().all(|b| b.is_ascii_digit()) {
            error("ply は空か 10 進の数字だけにしてください");
        }
        let evals = row.get("evals");
        if mean_eval(evals).is_err() {
            error("evals は整数をカンマで区切り、合計は i64、平均は i32 の範囲にしてください");
        }
        let key = row.get("key");
        let group = groups.entry(key.to_owned()).or_insert_with(|| PositionRows {
            first: at,
            ..Default::default()
        });
        let occurrences = group.moves.entry(row.get("move").to_owned()).or_default();
        if let Some(first) = occurrences.first() {
            error(&format!("同じ局面と手が {first} 行目と重なっています"));
        }
        occurrences.push(at);
        for scope in 0..2 {
            if scope == 1 && tier != "curated" {
                continue;
            }
            group.playable[scope] += usize::from(matches!(role, "main" | "alt"));
            group.mains[scope] += usize::from(role == "main");
        }
        if let Some((positions, moves)) = tiers.get_mut(tier) {
            positions.insert(key.to_owned());
            *moves += 1;
        }
        *roles.entry(role.to_owned()).or_default() += 1;
        *sources.entry(row.get("source").to_owned()).or_default() += 1;
    }
    if at == 1 {
        errors.push((1, "元データの 1 行目: 手の行がありません".into()));
    }
    for (key, group) in &groups {
        for (scope, name) in ["all", "curated"].iter().enumerate() {
            if group.playable[scope] > 0 && group.mains[scope] != 1 {
                errors.push((
                    group.first,
                    format!(
                        "元データの {} 行目: {key}: {name}: main がちょうど 1 つではありません",
                        group.first
                    ),
                ));
            }
        }
        let mut error = |message: &str| {
            errors.push((group.first, format!("元データの {} 行目: {key}: {message}", group.first)))
        };
        let parts: Vec<_> = key.split_whitespace().collect();
        if parts.len() != 3
            || parts[0].matches('K').count() != 1
            || parts[0].matches('k').count() != 1
        {
            error("key は両玉を 1 枚ずつ含む SFEN の 3 フィールドにしてください");
            continue;
        }
        let pos = match position_from_sfen(&format!("{key} 1")) {
            Ok(pos) => pos,
            Err(reason) => {
                error(&format!("key が不正です: {reason}"));
                continue;
            }
        };
        if strip_ply(&pos.to_sfen()) != key {
            error("key が正規形ではありません");
        }
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        let legal: BTreeSet<_> = legal.iter().map(|mv| mv.to_usi()).collect();
        for (mv, lines) in &group.moves {
            if !legal.contains(mv) {
                for at in lines {
                    errors.push((
                        *at,
                        format!("元データの {at} 行目: 合法手ではありません: {mv} ({key})"),
                    ));
                }
            }
        }
    }
    errors.sort_by_key(|(at, _)| *at);
    report.errors = errors.into_iter().map(|(_, message)| message).collect();
    if report.errors.is_empty() {
        writeln!(report.summary, "positions={}, moves={count}", groups.len())?;
        for (tier, (positions, moves)) in tiers {
            writeln!(report.summary, "tier {tier}: positions={}, moves={moves}", positions.len())?;
        }
        for (role, count) in roles {
            writeln!(report.summary, "role {role}: {count}")?;
        }
        for (source, count) in sources {
            writeln!(report.summary, "source {source}: {count}")?;
        }
    }
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::super::reply_graph::HIRATE_KEY;
    use super::*;

    fn row(mv: &str, role: &str, tier: &str) -> Row {
        Row::from_fields(&[
            ("key", HIRATE_KEY.into()),
            ("ply", "1".into()),
            ("move", mv.into()),
            ("role", role.into()),
            ("source", "自由な出典".into()),
            ("evals", "-1,+2,3".into()),
            ("tier", tier.into()),
            ("since", "test".into()),
        ])
    }

    fn errors(rows: &[Row]) -> Vec<String> {
        validate_table(format_master(rows).as_bytes()).unwrap().errors
    }

    #[test]
    fn cli_dispatches_validate_without_modifying_input() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            master: super::super::MasterArgs,
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        for (role, success) in [("main", true), ("alt", false)] {
            let text = format_master(&[row("7g7f", role, "curated")]);
            std::fs::write(&path, &text).unwrap();
            let cli =
                Cli::try_parse_from(["book_mine", "validate", "--master", path.to_str().unwrap()])
                    .unwrap();
            assert_eq!(super::super::run_master(cli.master).is_ok(), success);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        assert!(Cli::try_parse_from(["book_mine", "validate"]).is_err());
    }

    #[test]
    fn validation_and_audit_reader_reject_the_same_invalid_evals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        for evals in [
            "1.5,2e1",
            "2e1",
            " 20",
            "20 ",
            "1,",
            "2147483648",
            "-2147483649",
            "9223372036854775807,1",
            "-9223372036854775808,-1",
            "9223372036854775807,9223372036854775807",
        ] {
            let mut r = row("7g7f", "main", "wide");
            r.set("evals", evals.into());
            let text = format_master(&[r]);
            let report = validate_table(text.as_bytes()).unwrap();
            assert!(report.errors.iter().any(|e| e.contains("evals")), "{evals}: {report:?}");
            std::fs::write(&path, text).unwrap();
            assert!(read_master(&path).unwrap_err().to_string().contains("2 行目の evals"));
        }
        assert_eq!(mean_eval("2147483648,-2147483648").unwrap(), Some(0));
        assert_eq!(mean_eval("2147483647").unwrap(), Some(i32::MAX));
        assert_eq!(mean_eval("-2147483648").unwrap(), Some(i32::MIN));
    }

    #[test]
    fn invalid_utf8_is_reported_and_later_rows_are_checked() {
        let text = format_master(&[row("7g7f", "main", "wide")]);
        let (header, body) = text.split_once('\n').unwrap();
        for (bytes, at) in [
            ([header.as_bytes(), b"\n\xff\n", body.as_bytes(), b"bad\n"].concat(), 2),
            ([b"\xff\n".as_slice(), body.as_bytes(), b"bad\n"].concat(), 1),
        ] {
            let report = validate_table(bytes.as_slice()).unwrap();
            assert_eq!(report.errors.len(), 2, "{report:?}");
            assert_eq!(report.errors[0], format!("元データの {at} 行目: UTF-8 ではありません"));
            assert!(report.errors[1].contains("列の数"));
            assert!(
                read_rows(bytes.as_slice())
                    .and_then(|rows| rows.collect::<Result<Vec<_>>>())
                    .is_err()
            );
        }
        let bytes = [header.as_bytes(), b"\n\xff"].concat();
        assert_eq!(
            validate_table(bytes.as_slice()).unwrap().errors,
            ["元データの 2 行目: UTF-8 ではありません"]
        );
    }

    #[test]
    fn diagnostics_follow_line_order_and_preserve_order_within_a_line() {
        let mut first = row("7g7e", "alt", "curated");
        first.set("since", String::new());
        first.set("ply", "bad".into());
        let mut second = row("5i5h", "alt", "wide");
        second.set("key", "4k4/9/9/9/9/9/9/9/4K4 b -".into());
        second.set("evals", "bad".into());
        let found = errors(&[first, second]);
        assert_eq!(found.len(), 7, "{found:?}");
        for (message, expected) in found.iter().zip([
            "2 行目: since",
            "2 行目: ply",
            "all: main",
            "curated: main",
            "2 行目: 合法手",
            "3 行目: evals",
            "3 行目:",
        ]) {
            assert!(message.contains(expected), "{found:?}");
        }
        assert!(found[6].contains("all: main"));
    }

    #[test]
    fn malformed_rows_do_not_count_as_an_empty_table() {
        let text = format!("{}\nx\n", COLUMNS.join("\t"));
        let report = validate_table(text.as_bytes()).unwrap();
        assert_eq!(report.errors, ["元データの 2 行目の列の数が違います"]);
        assert!(report.summary.is_empty());
    }

    #[test]
    fn validation_errors_do_not_write_a_summary_to_stdout() {
        let mut stdout = Vec::new();
        let report = ValidationReport {
            errors: vec!["元データの 2 行目: evals が不正です".into()],
            summary: "positions=1, moves=1\n".into(),
        };
        assert!(write_validation_report(&report, &mut stdout).is_err());
        assert!(stdout.is_empty());
        let report =
            validate_table(format_master(&[row("7g7f", "main", "wide")]).as_bytes()).unwrap();
        write_validation_report(&report, &mut stdout).unwrap();
        assert_eq!(stdout, report.summary.as_bytes());
    }

    #[test]
    fn every_duplicate_illegal_move_has_a_line_number() {
        let found = errors(&[row("7g7e", "main", "wide"), row("7g7e", "record", "wide")]);
        assert_eq!(found.len(), 3);
        for at in [2, 3] {
            assert!(found.iter().any(|e| e.contains(&format!("{at} 行目: 合法手"))));
        }
    }

    #[test]
    fn line_endings_and_optional_promotion_are_validated() {
        let mut r = row("5d5c", "main", "wide");
        r.set("key", "4k4/9/9/4P4/9/9/9/9/4K4 b -".into());
        assert!(errors(&[r]).is_empty());
        let text = format_master(&[row("7g7f", "main", "wide")]);
        for text in [
            text.clone(),
            text.trim_end_matches('\n').to_owned(),
            text.replace('\n', "\r\n"),
        ] {
            assert!(validate_table(text.as_bytes()).unwrap().errors.is_empty());
        }
        let header = format!("{}\r", COLUMNS.join("\t"));
        assert!(
            validate_table(header.as_bytes())
                .unwrap()
                .errors
                .iter()
                .any(|e| e.contains("見出し"))
        );
    }

    #[test]
    fn rows_round_trip_preserves_bytes_and_column_order() {
        let rows = vec![row("7g7f", "main", "curated"), row("2g2f", "alt", "wide")];
        let text = format_master(&rows);
        assert_eq!(format_master(&parse_master(&text).unwrap()), text);
        assert_eq!(parse_master(&text).unwrap(), rows);
        assert_eq!(format_master(&parse_master(&text.replace('\n', "\r\n")).unwrap()), text);
        assert_eq!(format_nodes(1_000_000_000), "1e9");
        assert_eq!(format_nodes(230_000_000), "2.3e8");
    }

    #[test]
    fn valid_table_summary_includes_all_tiers_roles_and_sources() {
        let mut rows = vec![
            row("7g7f", "main", "curated"),
            row("2g2f", "alt", "wide"),
            row("5g5f", "record", "wide"),
        ];
        rows[0].set("ply", String::new());
        rows[1].set("evals", String::new());
        let report = validate_table(format_master(&rows).as_bytes()).unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            report.summary,
            "positions=1, moves=3\ntier curated: positions=1, moves=1\ntier wide: positions=1, moves=2\nrole alt: 1\nrole main: 1\nrole record: 1\nsource 自由な出典: 3\n"
        );
        assert!(errors(&[row("7g7f", "record", "curated")]).is_empty());
    }

    #[test]
    fn rejects_each_invalid_field() {
        for (column, value, expected) in [
            ("since", " \t", "列の数"),
            ("since", "  ", "since"),
            ("note", "a\rb", "CR"),
            ("note", "a\nb", "列の数"),
            ("role", "other", "role"),
            ("tier", "other", "tier"),
            ("ply", "-1", "ply"),
            ("ply", "+1", "ply"),
            ("ply", "１", "ply"),
            ("ply", " 1", "ply"),
            ("evals", "NaN", "evals"),
            ("evals", "inf", "evals"),
            ("evals", "1,", "evals"),
            ("evals", "1,,2", "evals"),
            ("move", "resign", "合法手"),
            ("move", "7g7e", "合法手"),
            ("move", "P*5e", "合法手"),
            ("key", "bad b -", "key"),
            ("key", "9/9/9/9/9/9/9/9/4K4 b -", "key"),
            ("key", "4k4/9/9/9/9/9/9/9/4K4 b - 1", "key"),
            ("key", "4k4/9/9/9/9/9/9/9/4K4 x -", "key"),
            ("key", "4k4/9/9/9/9/9/9/9/4K4 b 0P", "key"),
            ("key", "4k4/9/9/9/9/9/9/9/4K4 b 1P", "正規形"),
            ("key", "4k4/9/9/9/9/9/9/9/4K4  b -", "正規形"),
        ] {
            let mut r = row("7g7f", "main", "curated");
            r.set(column, value.into());
            let found = errors(&[r]);
            assert!(found.iter().any(|e| e.contains(expected)), "{column}={value:?}: {found:?}");
            assert!(found.iter().all(|e| e.contains("行目")), "{found:?}");
        }
    }

    #[test]
    fn rejects_header_column_count_and_empty_table() {
        for text in [String::new(), COLUMNS.join("\t"), "wrong\nx\n".into()] {
            assert!(!validate_table(text.as_bytes()).unwrap().errors.is_empty());
        }
        let text = format!(
            "wrong\n{}",
            format_master(&[row("7g7f", "main", "wide")]).split_once('\n').unwrap().1
        );
        let report = validate_table(text.as_bytes()).unwrap();
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("1 行目"));
    }

    #[test]
    fn reports_every_independent_error_and_both_main_scopes() {
        let mut r = row("7g7e", "alt", "curated");
        r.set("since", "".into());
        r.set("ply", "bad".into());
        r.set("evals", "bad".into());
        let report = errors(&[r, row("2g2e", "record", "wide")]);
        for expected in [
            "2 行目: since",
            "2 行目: ply",
            "2 行目: evals",
            "all: main",
            "curated: main",
            "2 行目: 合法手",
            "3 行目: 合法手",
        ] {
            assert!(report.iter().any(|e| e.contains(expected)), "{report:?}");
        }
        assert_eq!(report.len(), 7);
        let duplicate = errors(&[
            row("7g7f", "main", "wide"),
            row("7g7f", "record", "curated"),
        ]);
        assert_eq!(duplicate.len(), 1);
        assert!(duplicate[0].contains("3 行目") && duplicate[0].contains("2 行目"));
        let curated = errors(&[row("7g7f", "main", "wide"), row("2g2f", "alt", "curated")]);
        assert_eq!(curated.len(), 1);
        assert!(curated[0].contains("curated: main"));
        let mains = errors(&[
            row("7g7f", "main", "curated"),
            row("2g2f", "main", "curated"),
        ]);
        assert_eq!(mains.len(), 2);
    }

    #[test]
    fn legacy_readers_keep_distinct_validation_and_error_text() {
        let mut record = row("7g7f", "record", "curated");
        record.set("evals", "bad".into());
        let rows = [row("7g7f", "main", "wide"), record];
        let text = format_master(&rows);
        assert!(parse_master(&text).is_ok());
        assert_eq!(parse_master_rows(text.as_bytes()).unwrap().1, 1);
        assert!(validate(&rows).is_err());
        assert!(!errors(&rows).is_empty());
        let mut main = row("7g7f", "main", "curated");
        main.set("ply", "".into());
        let text = format_master(&[main]);
        assert!(parse_master(&text).is_ok());
        assert_eq!(
            parse_master_rows(text.as_bytes()).unwrap_err().to_string(),
            format!("main の無い局面があります: {HIRATE_KEY}")
        );
        let text = format_master(&[row("7g7f", "main", "wide"), row("2g2f", "main", "wide")]);
        assert_eq!(
            parse_master_rows(text.as_bytes()).unwrap_err().to_string(),
            "元データの 3 行目: 局面に main が 2 つあります"
        );
        let mut r = row("7g7f", "main", "wide");
        r.set("note", "a\rb".into());
        assert!(parse_master(&format_master(&[r])).is_ok());
        assert_eq!(parse_master("wrong").unwrap_err().to_string(), "元データの見出しが違います");
        let text = format!("{}\nx\n", COLUMNS.join("\t"));
        assert_eq!(
            parse_master(&text).unwrap_err().to_string(),
            "元データの 2 行目の列の数が違います"
        );
        let text = format_master(&[row("7g7f", "bad", "wide")]);
        assert_eq!(
            parse_master(&text).unwrap_err().to_string(),
            "元データの 2 行目の role が不正です"
        );
    }
}
