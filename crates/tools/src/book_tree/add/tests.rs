use super::super::reply_graph::HIRATE_KEY;
use super::*;
fn row(mv: &str, role: &str) -> Row {
    Row::from_fields(&[
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
        add: dir.join("add.tsv"),
        apply: false,
        replace: false,
        schema: None,
    }
}
#[test]
fn dry_run_apply_conflicts_and_main_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    let original = master_table::format_master(&[row("7g7f", "main")]);
    std::fs::write(&a.master, &original).unwrap();
    let mut conflict = row("7g7f", "alt");
    conflict.set("source", "pin".into());
    conflict.set("tier", "curated".into());
    std::fs::write(&a.add, master_table::format_master(&[conflict])).unwrap();
    let mut out = Vec::new();
    run_to(a, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("role: main -> alt, tier: wide -> curated, source: read -> pin"));
    assert!(out.ends_with("OK (dry run): rows 1 -> 1; no files written\n"));
    a = args(dir.path());
    std::fs::write(&a.add, master_table::format_master(&[row("2g2f", "alt")])).unwrap();
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
    new_main.set("evals", "1.5,2e1".into());
    std::fs::write(&a.add, master_table::format_master(&[new_main.clone(), row("7g7f", "alt")]))
        .unwrap();
    let mut out = Vec::new();
    run_to(a, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains(
        "replaced existing rows: 2\nchanged role: 2\nchanged evals: 1\nchanged note: 1\n"
    ));
    let a = args(dir.path());
    let merged = master_table::parse_master(&std::fs::read_to_string(a.master).unwrap()).unwrap();
    assert_eq!(merged, [new_main, row("7g7f", "alt")]);
}
#[test]
fn duplicate_additions_invalid_merge_and_same_file_never_write() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    let original = master_table::format_master(&[row("7g7f", "main")]);
    std::fs::write(&a.master, &original).unwrap();
    for additions in [
        vec![row("7g7f", "main"), row("7g7f", "main")],
        vec![row("2g2f", "main")],
        vec![row("bad", "alt"), row("worse", "alt")],
    ] {
        std::fs::write(&a.add, master_table::format_master(&additions)).unwrap();
        let mut a = args(dir.path());
        a.apply = true;
        let error = run_to(a, &mut Vec::new()).unwrap_err().to_string();
        if additions[0].get("move") == "bad" {
            assert!(error.contains("bad") && error.contains("worse"));
        }
        assert_eq!(std::fs::read_to_string(&args(dir.path()).master).unwrap(), original);
    }
    let mut a = args(dir.path());
    a.add = a.master.clone();
    assert!(run_to(a, &mut Vec::new()).is_err());
    let a = args(dir.path());
    std::fs::remove_file(&a.add).unwrap();
    std::fs::hard_link(&a.master, &a.add).unwrap();
    assert!(run_to(a, &mut Vec::new()).is_err());
}
#[test]
fn schema_checks_original_and_merged_sources() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    a.schema = Some(dir.path().into());
    a.apply = true;
    std::fs::write(dir.path().join("sources.tsv"), "source\tdescription\nread\t読み\n").unwrap();
    let original = master_table::format_master(&[row("7g7f", "main")]);
    std::fs::write(&a.master, &original).unwrap();
    let mut alt = row("2g2f", "alt");
    alt.set("source", "unknown".into());
    std::fs::write(&a.add, master_table::format_master(&[alt])).unwrap();
    assert!(run_to(a, &mut Vec::new()).unwrap_err().to_string().contains("unknown source"));
    assert_eq!(std::fs::read_to_string(args(dir.path()).master).unwrap(), original);
}
