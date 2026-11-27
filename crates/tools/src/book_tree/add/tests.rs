use super::super::reply_graph::HIRATE_KEY;
use super::*;
fn row(mv: &str, role: &str) -> Row {
    crate::book_tree::master_table::test_row(&[
        ("key", HIRATE_KEY.into()),
        ("ply", "1".into()),
        ("move", mv.into()),
        ("role", role.into()),
        ("tier", "wide".into()),
        ("source", "read".into()),
        ("since", "test".into()),
    ])
}
fn args(dir: &Path) -> AddArgs {
    AddArgs {
        master: dir.join("master.tsv"),
        add: Some(dir.join("add.tsv")),
        line: None,
        from: None,
        to: None,
        source: None,
        since: None,
        note: None,
        apply: false,
        replace: false,
        schema: None,
    }
}
#[test]
fn dry_run_apply_conflicts_and_main_replacement() {
    let dir = tempfile::tempdir().unwrap();
    crate::book_tree::schema::test_schema(dir.path());
    let mut a = args(dir.path());
    let original = master_table::format_master(&[row("7g7f", "main")]);
    std::fs::write(&a.master, &original).unwrap();
    let mut conflict = row("7g7f", "alt");
    conflict.set("source", "pin".into());
    conflict.set("tier", "curated".into());
    std::fs::write(a.add.as_ref().unwrap(), master_table::format_master(&[conflict])).unwrap();
    let mut out = Vec::new();
    run_to(a, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("role: main -> alt, tier: wide -> curated, source: read -> pin"));
    assert!(out.ends_with("OK (dry run): rows 1 -> 1; no files written\n"));
    a = args(dir.path());
    std::fs::write(a.add.as_ref().unwrap(), master_table::format_master(&[row("2g2f", "alt")]))
        .unwrap();
    run_to(a, &mut Vec::new()).unwrap();
    a = args(dir.path());
    assert_eq!(std::fs::read_to_string(&a.master).unwrap(), original);
    a.apply = true;
    run_to(a, &mut Vec::new()).unwrap();
    a = args(dir.path());
    a.apply = true;
    a.replace = true;
    let mut new_main = row("2g2f", "main");
    new_main.set("note", "選択理由".into());
    new_main.set("cond", "unknown".into());
    new_main.set("value_kind", "unknown".into());
    new_main.set("evals", "1.5,2e1".into());
    std::fs::write(
        a.add.as_ref().unwrap(),
        master_table::format_master(&[new_main.clone(), row("7g7f", "alt")]),
    )
    .unwrap();
    let mut out = Vec::new();
    let original = std::fs::read(&a.master).unwrap();
    assert!(run_to(a, &mut out).unwrap_err().to_string().contains("evals"));
    assert_eq!(std::fs::read(args(dir.path()).master).unwrap(), original);
    new_main.set("evals", "1,20".into());
    let mut a = args(dir.path());
    a.apply = true;
    a.replace = true;
    std::fs::write(
        a.add.as_ref().unwrap(),
        master_table::format_master(&[new_main.clone(), row("7g7f", "alt")]),
    )
    .unwrap();
    run_to(a, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains(
        "replaced existing rows: 2\nchanged role: 2\nchanged evals: 1\nchanged cond: 1\nchanged value_kind: 1\nchanged note: 1\n"
    ));
    let a = args(dir.path());
    let merged = master_table::parse_master(&std::fs::read_to_string(a.master).unwrap()).unwrap();
    assert_eq!(merged, [new_main, row("7g7f", "alt")]);
}
#[test]
fn duplicate_additions_invalid_merge_and_same_file_never_write() {
    let dir = tempfile::tempdir().unwrap();
    crate::book_tree::schema::test_schema(dir.path());
    let a = args(dir.path());
    let original = master_table::format_master(&[row("7g7f", "main")]);
    std::fs::write(&a.master, &original).unwrap();
    for additions in [
        vec![row("7g7f", "main"), row("7g7f", "main")],
        vec![row("2g2f", "main")],
        vec![row("bad", "alt"), row("worse", "alt")],
    ] {
        std::fs::write(a.add.as_ref().unwrap(), master_table::format_master(&additions)).unwrap();
        let mut a = args(dir.path());
        a.apply = true;
        let error = run_to(a, &mut Vec::new()).unwrap_err().to_string();
        if additions[0].get("move") == "bad" {
            assert!(error.contains("bad") && error.contains("worse"));
        }
        assert_eq!(std::fs::read_to_string(&args(dir.path()).master).unwrap(), original);
    }
    let mut a = args(dir.path());
    a.add = Some(a.master.clone());
    assert!(run_to(a, &mut Vec::new()).is_err());
    let a = args(dir.path());
    std::fs::remove_file(a.add.as_ref().unwrap()).unwrap();
    std::fs::hard_link(&a.master, a.add.as_ref().unwrap()).unwrap();
    assert!(run_to(a, &mut Vec::new()).is_err());
}
#[test]
fn schema_checks_original_and_merged_sources() {
    let dir = tempfile::tempdir().unwrap();
    crate::book_tree::schema::test_schema(dir.path());
    let mut a = args(dir.path());
    a.schema = Some(dir.path().into());
    a.apply = true;
    std::fs::write(
        dir.path().join("sources.tsv"),
        "source\tdescription\treplaceable\nread\t読み\t0\n",
    )
    .unwrap();
    let original = master_table::format_master(&[row("7g7f", "main")]);
    std::fs::write(&a.master, &original).unwrap();
    let mut alt = row("2g2f", "alt");
    alt.set("source", "unknown".into());
    std::fs::write(a.add.as_ref().unwrap(), master_table::format_master(&[alt])).unwrap();
    assert!(run_to(a, &mut Vec::new()).unwrap_err().to_string().contains("unknown source"));
    assert_eq!(std::fs::read_to_string(args(dir.path()).master).unwrap(), original);
}

#[test]
fn replacement_reports_tier_changes_and_curated_column_changes() {
    for (old_tier, new_tier) in [
        ("wide", "curated"),
        ("curated", "wide"),
        ("curated", "curated"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        crate::book_tree::schema::test_schema(dir.path());
        let mut a = args(dir.path());
        a.replace = true;
        a.apply = true;
        let mut old = row("7g7f", "main");
        old.set("tier", old_tier.into());
        old.set("note", "old".into());
        old.set("evals", "10".into());
        old.set("cond", "unknown".into());
        old.set("value_kind", "unknown".into());
        let mut new = old.clone();
        new.set("tier", new_tier.into());
        new.set("note", "new".into());
        new.set("evals", "20".into());
        std::fs::write(&a.master, master_table::format_master(&[old])).unwrap();
        std::fs::write(a.add.as_ref().unwrap(), master_table::format_master(&[new.clone()]))
            .unwrap();
        let mut out = Vec::new();
        run_to(a, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        let line = out.lines().find(|l| l.starts_with("replaced: ")).unwrap();
        assert!(line.starts_with(&format!("replaced: {HIRATE_KEY} | 7g7f: ")));
        assert!(line.contains("evals: 10 -> 20"));
        assert!(line.contains("note: old -> new"));
        if old_tier != new_tier {
            assert!(line.contains(&format!("tier: {old_tier} -> {new_tier}")));
        }
        assert_eq!(
            std::fs::read_to_string(args(dir.path()).master).unwrap(),
            master_table::format_master(&[new])
        );
    }
}

#[test]
fn counts_use_schema_order_then_unknown_sources_or_lexical_order() {
    let mut rows = Vec::new();
    for source in ["zeta", "ext", "alpha"] {
        let mut r = row("7g7f", "main");
        r.set("source", source.into());
        rows.push(r);
    }
    let dir = tempfile::tempdir().unwrap();
    crate::book_tree::schema::test_schema(dir.path());
    std::fs::write(
        dir.path().join("sources.tsv"),
        "source\tdescription\treplaceable\nzeta\tZ\t0\nunused\tU\t0\n",
    )
    .unwrap();
    for (schema, expected) in [
        (Schema::default(), "source: alpha=1, ext=1, zeta=1"),
        (
            Schema::load(Some(dir.path())).unwrap(),
            "source: zeta=1, unused=0, alpha=1, ext=1",
        ),
    ] {
        let mut out = Vec::new();
        export::print_counts(&rows, &schema, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().lines().any(|line| line == expected));
    }
}

#[test]
fn identical_curated_replacement_has_no_empty_detail() {
    let d = tempfile::tempdir().unwrap();
    let mut a = args(d.path());
    a.replace = true;
    let mut row = row("7g7f", "main");
    row.set("tier", "curated".into());
    let text = master_table::format_master(&[row]);
    std::fs::write(&a.master, &text).unwrap();
    std::fs::write(a.add.as_ref().unwrap(), &text).unwrap();
    let mut out = Vec::new();
    run_to(a, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(!out.contains("replaced:"));
    assert!(out.contains("replaced existing rows: 1"));
}

#[derive(clap::Parser)]
struct AddCli {
    #[command(flatten)]
    master: super::super::MasterArgs,
}

const FIXED_LINE: &str = "startpos moves 7g7f 3c3d 8h2b+ 3a2b B*4e B*5e";

fn line_argv(dir: &Path, extra: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "book_mine".into(),
        "add".into(),
        "--master".into(),
        dir.join("master.tsv").display().to_string(),
        "--schema".into(),
        dir.display().to_string(),
    ];
    argv.extend(extra.iter().map(|s| s.to_string()));
    argv
}

fn line_args(
    dir: &Path,
    line: &str,
    from: &str,
    to: &str,
    source: &str,
    since: &str,
    note: &str,
) -> AddArgs {
    use clap::Parser;
    let cli = AddCli::try_parse_from(line_argv(
        dir,
        &[
            "--line", line, "--from", from, "--to", to, "--source", source, "--since", since,
            "--note", note,
        ],
    ))
    .unwrap();
    let super::super::MasterCommand::Add(args) = cli.master.command else {
        unreachable!()
    };
    args
}

fn line_master(dir: &Path, rows: &[Row]) -> String {
    crate::book_tree::schema::test_schema(dir);
    std::fs::write(
        dir.join("sources.tsv"),
        "source\tdescription\treplaceable\nread\t読み\t0\npin\t固定\t0\n",
    )
    .unwrap();
    let original = master_table::format_master(rows);
    std::fs::write(dir.join("master.tsv"), &original).unwrap();
    original
}

#[test]
fn master_add_line_matches_fixed_rows_fixture_and_only_writes_on_apply() {
    let dir = tempfile::tempdir().unwrap();
    let original = line_master(dir.path(), &[row("7g7f", "main")]);
    let full_line = format!("position {FIXED_LINE}");
    let mut args = line_args(dir.path(), &full_line, "2", "6", "pin", "fixture-tag", "固定手順");
    let schema = Schema::load(args.schema.as_deref()).unwrap();
    let expected = include_str!("fixed_rows.tsv");
    assert_eq!(
        master_table::format_master(&line_rows(&full_line, &args, &schema).unwrap()),
        expected
    );

    let mut out = Vec::new();
    run_to(args, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("add: 2 手目 後手 3c3d\n"));
    assert!(out.contains("add: 6 手目 後手 B*5e\n"));
    assert!(out.contains("line: add=5, skip=0, blocked=0\n"));
    let master = dir.path().join("master.tsv");
    assert_eq!(std::fs::read_to_string(&master).unwrap(), original);

    args = line_args(dir.path(), FIXED_LINE, "1", "6", "pin", "fixture-tag", "固定手順");
    args.apply = true;
    let mut out = Vec::new();
    run_to(args, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("skip: 1 手目 先手 7g7f\n"));
    assert!(out.contains("line: add=5, skip=1, blocked=0\n"));
    let mut expected = master_table::parse_master(expected).unwrap();
    expected.push(row("7g7f", "main"));
    sort_rows(&mut expected);
    assert_eq!(
        std::fs::read_to_string(&master).unwrap(),
        master_table::format_master(&expected)
    );

    args = line_args(dir.path(), FIXED_LINE, "1", "6", "pin", "別のタグ", "別の理由");
    args.apply = true;
    expected.reverse();
    let before = master_table::format_master(&expected);
    std::fs::write(&master, &before).unwrap();
    let mut out = Vec::new();
    run_to(args, &mut out).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("line: add=0, skip=6, blocked=0\n"));
    assert_eq!(std::fs::read_to_string(&master).unwrap(), before);
}

#[test]
fn master_add_line_reports_all_conflicting_positions_and_keeps_master() {
    let dir = tempfile::tempdir().unwrap();
    let white_key = super::super::model::child(HIRATE_KEY, "7g7f").unwrap();
    let mut white = row("8c8d", "main");
    white.set("key", white_key);
    white.set("ply", "2".into());
    white.set("evals", "42,-10".into());
    white.set("cond", "unknown".into());
    white.set("value_kind", "unknown".into());
    for (mut rows, details) in [
        (vec![row("2g2f", "main")], "2g2f role=main"),
        (vec![row("2g2f", "main"), row("7g7f", "alt")], "7g7f role=alt"),
        (vec![row("7g7f", "bench")], "7g7f role=bench"),
        (vec![row("7g7f", "record")], "7g7f role=record"),
        (vec![row("7g7f", "main"), row("2g2f", "record")], "2g2f role=record"),
    ] {
        rows.push(white.clone());
        let original = line_master(dir.path(), &rows);
        let mut args = line_args(dir.path(), FIXED_LINE, "1", "3", "pin", "test", "理由");
        args.apply = true;
        let mut out = Vec::new();
        let error = run_to(args, &mut out).unwrap_err().to_string();
        assert!(error.contains("1 手目 先手 7g7f") && error.contains(details), "{error}");
        assert!(error.contains("2 手目 後手 3c3d: 8c8d role=main evals=\"42,-10\""), "{error}");
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("blocked: 1 手目 先手 7g7f\n"));
        assert!(out.contains("add: 3 手目 先手 8h2b+\n"));
        assert!(out.contains("line: add=1, skip=0, blocked=2\n"));
        assert_eq!(std::fs::read_to_string(dir.path().join("master.tsv")).unwrap(), original);
    }
}

#[test]
fn master_add_line_uses_direct_keys_and_classifies_repeated_positions() {
    let dir = tempfile::tempdir().unwrap();
    let mut flipped = row("7g7f", "main");
    let after_7g7f = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2";
    let flipped_key = rshogi_book::flipped_key(after_7g7f).unwrap();
    flipped.set("key", strip_ply(&flipped_key).into());
    let original = line_master(dir.path(), &[flipped]);
    for (line, to, fails, expected) in [
        ("startpos moves 7g7f 3c3d", "2", false, "line: add=1, skip=0, blocked=0"),
        (
            "startpos moves 5i5h 5a5b 5h5i 5b5a 5i5h",
            "5",
            false,
            "line: add=4, skip=1, blocked=0",
        ),
        (
            "startpos moves 5i5h 5a5b 5h5i 5b5a 7g7f",
            "5",
            true,
            "line: add=4, skip=0, blocked=1",
        ),
    ] {
        let from = if to == "2" { "2" } else { "1" };
        let mut args = line_args(dir.path(), line, from, to, "pin", "test", "理由");
        args.apply = fails;
        let mut out = Vec::new();
        assert_eq!(run_to(args, &mut out).is_err(), fails);
        assert!(String::from_utf8(out).unwrap().contains(expected), "{line}");
        assert_eq!(std::fs::read_to_string(dir.path().join("master.tsv")).unwrap(), original);
    }
}

#[test]
fn master_add_line_rejects_invalid_input_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let original = line_master(dir.path(), &[row("7g7f", "main")]);
    for (line, from, to, source, since, note, expected) in [
        ("startpos moves 7g7f 3c3d 7f7d", "2", "2", "pin", "test", "理由", "3 手目 7f7d"),
        (
            "startpos moves 7g7f 3c3d 8h2b",
            "3",
            "3",
            "pin",
            "test",
            "理由",
            "成れる角・飛・歩の不成は main・alt にできません",
        ),
        ("startpos moves 7g7f 3c3d pass", "2", "2", "pin", "test", "理由", "3 手目 pass"),
        ("startpos moves 7g7f 3c3dxxx", "1", "1", "pin", "test", "理由", "2 手目 3c3dxxx"),
        (FIXED_LINE, "2", "7", "pin", "test", "理由", "範囲"),
        (FIXED_LINE, "0", "1", "pin", "test", "理由", "範囲"),
        (FIXED_LINE, "3", "2", "pin", "test", "理由", "範囲"),
        (FIXED_LINE, "1", "1", "unknown", "test", "理由", "unknown source"),
        (FIXED_LINE, "2", "2", "pin", "test", "理由\t本文", "--note"),
        (FIXED_LINE, "2", "2", "pin", "test", "", "--note"),
        (FIXED_LINE, "2", "2", "pin", "test", "理由\n本文", "--note"),
        (FIXED_LINE, "2", "2", "pin", "", "理由", "--since"),
        (FIXED_LINE, "2", "2", "pin", "test\r", "理由", "--since"),
    ] {
        let mut args = line_args(dir.path(), line, from, to, source, since, note);
        args.apply = true;
        let error = format!("{:#}", run_to(args, &mut Vec::new()).unwrap_err());
        assert!(error.contains(expected), "{error}");
        assert_eq!(std::fs::read_to_string(dir.path().join("master.tsv")).unwrap(), original);
    }
}

#[test]
fn master_add_line_cli_requires_complete_options_and_rejects_mixed_modes() {
    use clap::{Parser, error::ErrorKind};
    let dir = tempfile::tempdir().unwrap();
    let mut valid = line_argv(
        dir.path(),
        &[
            "--line", FIXED_LINE, "--from", "2", "--to", "3", "--source", "pin", "--since", "test",
            "--note", "理由",
        ],
    );
    assert!(AddCli::try_parse_from(&valid).is_ok());
    for name in [
        "--from", "--to", "--source", "--since", "--note", "--schema",
    ] {
        let at = valid.iter().position(|arg| arg == name).unwrap();
        let mut incomplete = valid.clone();
        incomplete.drain(at..at + 2);
        let Err(error) = AddCli::try_parse_from(incomplete) else {
            panic!("{name} が必須ではありません")
        };
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }
    for flags in [vec!["--add", "add.tsv"], vec!["--replace"]] {
        let original_len = valid.len();
        valid.extend(flags.iter().map(|flag| flag.to_string()));
        let Err(error) = AddCli::try_parse_from(&valid) else {
            panic!("{flags:?} が併用できます")
        };
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
        valid.truncate(original_len);
    }
    let tsv = line_argv(dir.path(), &["--add", "add.tsv", "--replace"]);
    assert!(AddCli::try_parse_from(&tsv).is_ok());
    let mut mixed = tsv;
    mixed.extend(["--source".into(), "pin".into()]);
    assert!(AddCli::try_parse_from(&mixed).is_err());
}
