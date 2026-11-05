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
            assert_eq!(r.get("note"), "既存。held: 20 手目より前の main の替わりを保留に戻した");
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
    assert_eq!(lines[1], "19\tb\t4k4/9/9/9/9/9/9/9/4K4 b -\t5i5h\tread\twide\t\t5i4h\t50");
    assert_eq!(lines[2], "19\tw\t4k4/9/9/9/9/9/9/9/4K4 w -\t5a5b\tread\twide\t-100\t5a4b\t200");
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        "対象局面: 2 (先手番: 1, 後手番: 1)\n書き戻した局面: 0\n"
    );
    run(super::tests::args(dir.path(), &before, &current, false)).unwrap();
    assert_eq!(std::fs::read_to_string(out).unwrap(), table);
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
    let cli = Cli::try_parse_from([
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
    ])
    .unwrap();
    let crate::book_tree::MasterCommand::HoldEarly(args) = cli.master.command else {
        panic!("hold-early")
    };
    assert_eq!(args.below_ply, 20);
    assert!(!args.apply);
}
