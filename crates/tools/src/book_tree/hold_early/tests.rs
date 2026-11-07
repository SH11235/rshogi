use super::*;
use crate::book_tree::master_table::test_row;

fn row(side: &str, ply: &str, mv: &str, role: &str, tier: &str, eval: &str) -> Row {
    test_row(&[
        ("key", format!("4k4/9/9/9/9/9/9/9/4K4 {side} -")),
        ("ply", ply.into()),
        ("move", mv.into()),
        ("role", role.into()),
        ("tier", tier.into()),
        ("evals", eval.into()),
        ("source", "read".into()),
        ("since", "original".into()),
        ("note", "既存".into()),
    ])
}

fn args(dir: &std::path::Path, before: &[Row], current: &[Row], apply: bool) -> HoldEarlyArgs {
    let args = HoldEarlyArgs {
        before: dir.join("before.tsv"),
        master: dir.join("moves.tsv"),
        below_ply: 20,
        continuation: None,
        hold_keys: None,
        schema: None,
        since: "held".into(),
        out: dir.join("held.tsv"),
        apply,
    };
    std::fs::write(&args.before, format_master(before)).unwrap();
    std::fs::write(&args.master, format_master(current)).unwrap();
    args
}

#[test]
fn hold_early_restores_existing_roles_and_missing_row() {
    for role in ["alt", "bench", "record", "missing"] {
        let dir = tempfile::tempdir().unwrap();
        let old = row("b", "19", "5i5h", "main", "curated", "100");
        let new = row("b", "19", "5i4h", "main", "curated", "200");
        let mut current = vec![new];
        if role != "missing" {
            current.push(row("b", "19", "5i5h", role, "wide", "150"));
        }
        let args = args(dir.path(), &[old], &current, true);
        let master = args.master.clone();
        let mut stdout = Vec::new();
        run_to(args, &mut stdout).unwrap();
        let rows = parse_master(&std::fs::read_to_string(master).unwrap()).unwrap();
        let restored = rows.iter().find(|r| r.get("move") == "5i5h").unwrap();
        assert_eq!(restored.get("role"), "main");
        assert_eq!(restored.get("tier"), "curated");
        assert_eq!(restored.get("evals"), if role == "missing" { "100" } else { "150" });
        assert_eq!(rows[0].get("role"), "alt");
        assert_eq!(rows[0].get("tier"), "wide");
        for r in &rows {
            assert_eq!(r.get("note"), "既存; held: 20 手目より前、読みが浅いので保留 (今の main)");
            assert_eq!(r.get("since"), "original");
        }
        assert!(String::from_utf8(stdout).unwrap().contains("書き戻した局面: 1"));
    }
}

#[test]
fn hold_early_excludes_boundary_new_positions_and_unknown_ply() {
    for ply in ["20", "", "999999999999999999999999999"] {
        let dir = tempfile::tempdir().unwrap();
        let before = [row("b", ply, "5i5h", "main", "wide", "")];
        let current = [
            row("b", "1", "5i4h", "main", "wide", ""),
            row("w", "2", "5a5b", "main", "wide", ""),
        ];
        let args = args(dir.path(), &before, &current, true);
        let master = args.master.clone();
        let out = args.out.clone();
        run(args).unwrap();
        assert_eq!(std::fs::read_to_string(master).unwrap(), format_master(&current));
        assert_eq!(std::fs::read_to_string(out).unwrap().lines().count(), 1);
    }
}

#[test]
fn hold_early_dry_run_values_order_and_repeatability() {
    let dir = tempfile::tempdir().unwrap();
    let before = [
        row("w", "19", "5a5b", "main", "wide", "100,101"),
        row("b", "19", "5i5h", "main", "wide", ""),
    ];
    let current = [
        row("w", "19", "5a4b", "main", "wide", "-200"),
        row("b", "19", "5i4h", "main", "wide", "50"),
    ];
    let args = args(dir.path(), &before, &current, false);
    let master = args.master.clone();
    let out = args.out.clone();
    let mut stdout = Vec::new();
    run_to(args, &mut stdout).unwrap();
    assert_eq!(std::fs::read_to_string(&master).unwrap(), format_master(&current));
    let table = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<_> = table.lines().collect();
    assert_eq!(
        lines[0],
        "ply\tside\tkey\told_main\told_source\told_tier\told_value_black\tnew_main\tnew_value_black\told_depth\tnew_depth\tclass"
    );
    assert_eq!(
        lines[1],
        "19\tb\t4k4/9/9/9/9/9/9/9/4K4 b -\t5i5h\tread\twide\t\t5i4h\t50\t0\t0\thold"
    );
    assert_eq!(
        lines[2],
        "19\tw\t4k4/9/9/9/9/9/9/9/4K4 w -\t5a5b\tread\twide\t-100\t5a4b\t200\t0\t0\thold"
    );
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        "対象局面: 2 (先手番: 1, 後手番: 1)\n分類: hold 2, replace 0\n書き戻した局面: 0\n"
    );
    run(super::tests::args(dir.path(), &before, &current, false)).unwrap();
    assert_eq!(std::fs::read_to_string(out).unwrap(), table);
}

fn branch(rows: &mut Vec<Row>, first: &str, moves: &[&str]) {
    let mut position = position_from_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 50").unwrap();
    apply_usi_move(&mut position, first).unwrap();
    for mv in moves {
        let mut next = row("b", "50", mv, "main", "wide", "");
        next.set("key", strip_ply(&position.to_sfen()).into());
        rows.push(next);
        apply_usi_move(&mut position, mv).unwrap();
    }
}

#[test]
fn hold_early_classifies_continuations_before_applying() {
    for (old_length, new_length, continuation, forced, replace) in [
        (2, 2, Some(2), false, true),
        (1, 2, Some(2), false, false),
        (2, 1, Some(2), false, false),
        (2, 2, Some(2), true, false),
        (2, 2, None, false, false),
        (0, 0, Some(0), false, true),
        (2, 2, Some(61), false, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let before = [row("b", "19", "5i5h", "main", "wide", "100")];
        let mut current = vec![
            row("b", "19", "5i4h", "main", "wide", "200"),
            row("b", "19", "5i5h", "alt", "wide", "150"),
        ];
        branch(&mut current, "5i5h", &["5a5b", "5h5g"][..old_length]);
        branch(&mut current, "5i4h", &["5a5b", "4h4g"][..new_length]);
        let mut args = args(dir.path(), &before, &current, true);
        args.continuation = continuation;
        if forced {
            let path = dir.path().join("keys.txt");
            std::fs::write(&path, format!("# 保留する局面\n\n {} \n", before[0].get("key")))
                .unwrap();
            args.hold_keys = Some(path);
        }
        let master = args.master.clone();
        let out = args.out.clone();
        let mut stdout = Vec::new();
        run_to(args, &mut stdout).unwrap();
        let table = std::fs::read_to_string(out).unwrap();
        let class = if replace { "replace" } else { "hold" };
        assert!(
            table
                .lines()
                .nth(1)
                .unwrap()
                .ends_with(&format!("\t{old_length}\t{new_length}\t{class}"))
        );
        let updated = std::fs::read_to_string(master).unwrap();
        if replace {
            assert_eq!(updated, format_master(&current));
        } else {
            let rows = parse_master(&updated).unwrap();
            assert_eq!(rows[0].get("role"), "alt");
            assert_eq!(rows[1].get("role"), "main");
            assert_eq!(&rows[2..], &current[2..]);
        }
        let stdout = String::from_utf8(stdout).unwrap();
        assert!(stdout.contains(if replace {
            "分類: hold 0, replace 1"
        } else {
            "分類: hold 1, replace 0"
        }));
        assert!(stdout.contains(if replace {
            "書き戻した局面: 0"
        } else {
            "書き戻した局面: 1"
        }));
    }
}

#[test]
fn hold_early_restores_non_curated_tier_and_empty_note() {
    let dir = tempfile::tempdir().unwrap();
    let before = [row("b", "19", "5i5h", "main", "wide", "100")];
    let mut current = [
        row("b", "19", "5i4h", "main", "wide", "200"),
        row("b", "19", "5i5h", "bench", "curated", "150"),
    ];
    for row in &mut current {
        row.set("note", String::new());
    }
    let args = args(dir.path(), &before, &current, true);
    let master = args.master.clone();
    run(args).unwrap();
    let rows = parse_master(&std::fs::read_to_string(master).unwrap()).unwrap();
    assert_eq!(rows[1].get("tier"), "wide");
    for row in rows {
        assert_eq!(row.get("note"), "held: 20 手目より前、読みが浅いので保留 (今の main)");
    }
}

#[test]
fn hold_keys_does_not_change_unchanged_positions() {
    let dir = tempfile::tempdir().unwrap();
    let current = [row("b", "19", "5i5h", "main", "wide", "")];
    let mut args = args(dir.path(), &current, &current, true);
    let path = dir.path().join("keys.txt");
    std::fs::write(&path, current[0].get("key")).unwrap();
    args.hold_keys = Some(path);
    let master = args.master.clone();
    run(args).unwrap();
    assert_eq!(std::fs::read_to_string(master).unwrap(), format_master(&current));
}

#[test]
fn hold_keys_is_protected_from_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let current = [row("b", "19", "5i5h", "main", "wide", "")];
    for collide_master in [false, true] {
        let mut args = args(dir.path(), &current, &current, true);
        let path = if collide_master {
            args.master.clone()
        } else {
            args.out.clone()
        };
        if !collide_master {
            std::fs::write(&path, current[0].get("key")).unwrap();
        }
        let original = std::fs::read(&path).unwrap();
        args.hold_keys = Some(path.clone());
        assert!(run(args).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}

#[test]
fn continuation_stops_when_main_returns_to_a_seen_position() {
    let mut rows = vec![row("b", "19", "5i5h", "main", "wide", "")];
    branch(&mut rows, "5i5h", &["5a5b", "5h5i", "5b5a"]);
    let mains = rows.iter().enumerate().map(|(i, r)| (r.get("key").to_owned(), i)).collect();
    assert_eq!(continuation_depth(rows[0].get("key"), "5i5h", &rows, &mains).unwrap(), 3);
}

#[test]
fn hold_early_uses_master_before_any_holds() {
    let dir = tempfile::tempdir().unwrap();
    let mut current = vec![
        row("b", "2", "5i4h", "main", "wide", ""),
        row("b", "2", "5i5h", "alt", "wide", ""),
    ];
    branch(&mut current, "5i5h", &["5a5b", "5h5g"]);
    branch(&mut current, "5i4h", &["5a5b", "4h4g"]);
    let mut old_child = current[2].clone();
    old_child.set("move", "5a4b".into());
    old_child.set("ply", "1".into());
    let before = [row("b", "2", "5i5h", "main", "wide", ""), old_child];
    let mut args = args(dir.path(), &before, &current, true);
    args.continuation = Some(2);
    let master = args.master.clone();
    let out = args.out.clone();
    let mut stdout = Vec::new();
    run_to(args, &mut stdout).unwrap();
    let table = std::fs::read_to_string(out).unwrap();
    let lines: Vec<_> = table.lines().collect();
    assert!(lines[1].ends_with("\t0\t1\thold"));
    assert!(lines[2].ends_with("\t2\t2\treplace"));
    let rows = parse_master(&std::fs::read_to_string(master).unwrap()).unwrap();
    assert_eq!(&rows[..2], &current[..2]);
    assert_eq!(rows[2].get("role"), "alt");
    assert_eq!(rows.last().unwrap().get("move"), "5a4b");
    assert!(String::from_utf8(stdout).unwrap().contains("分類: hold 1, replace 1"));
}

#[test]
fn continuation_matches_hands_and_turn_without_ply() {
    let key = "4k4/9/9/9/9/9/9/4p4/4K4 b -";
    let mut position = position_from_sfen(&format!("{key} 50")).unwrap();
    apply_usi_move(&mut position, "5i5h").unwrap();
    let child = strip_ply(&position.to_sfen()).to_owned();
    let mut next = row("w", "99", "5a5b", "main", "wide", "");
    next.set("key", child.clone());
    let rows = [next];
    for (lookup, expected) in [
        (child.clone(), 1),
        (child.replace(" w P", " w -"), 0),
        (child.replace(" w P", " b P"), 0),
    ] {
        let mains = BTreeMap::from([(lookup, 0)]);
        assert_eq!(continuation_depth(key, "5i5h", &rows, &mains).unwrap(), expected);
    }
}

#[test]
fn hold_early_invalid_result_does_not_write() {
    let dir = tempfile::tempdir().unwrap();
    let before = [row("b", "19", "5i5h", "main", "wide", "")];
    let current = [row("b", "19", "5i4h", "main", "curated", "")];
    let args = args(dir.path(), &before, &current, true);
    let master = args.master.clone();
    let out = args.out.clone();
    assert!(run(args).unwrap_err().to_string().contains("curated: main"));
    assert_eq!(std::fs::read_to_string(master).unwrap(), format_master(&current));
    assert!(!out.exists());
}

#[test]
fn hold_early_rejects_output_input_collision() {
    let dir = tempfile::tempdir().unwrap();
    let before = [row("b", "19", "5i5h", "main", "wide", "")];
    let current = [row("b", "19", "5i4h", "main", "wide", "")];
    for target in ["before.tsv", "moves.tsv"] {
        let mut args = args(dir.path(), &before, &current, false);
        args.out = dir.path().join(target);
        assert!(run(args).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("before.tsv")).unwrap(),
            format_master(&before)
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("moves.tsv")).unwrap(),
            format_master(&current)
        );
    }
}

#[test]
fn hold_early_cli_defaults() {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        master: crate::book_tree::MasterArgs,
    }
    let base = [
        "master",
        "hold-early",
        "--before",
        "before.tsv",
        "--master",
        "moves.tsv",
        "--since",
        "tag",
        "--out",
        "held.tsv",
    ];
    let cli = Cli::try_parse_from(base).unwrap();
    let crate::book_tree::MasterCommand::HoldEarly(args) = cli.master.command else {
        panic!("hold-early")
    };
    assert_eq!(args.below_ply, 20);
    assert!(!args.apply);
    assert_eq!(args.continuation, None);
    assert_eq!(args.hold_keys, None);
    let cli = Cli::try_parse_from(base.into_iter().chain([
        "--continuation",
        "6",
        "--hold-keys",
        "keys.txt",
        "--apply",
    ]))
    .unwrap();
    let crate::book_tree::MasterCommand::HoldEarly(args) = cli.master.command else {
        panic!("hold-early")
    };
    assert_eq!(args.continuation, Some(6));
    assert_eq!(args.hold_keys, Some(PathBuf::from("keys.txt")));
    assert!(args.apply);
}
