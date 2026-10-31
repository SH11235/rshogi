use super::*;
#[test]
fn schema_is_explicit_and_ordered() {
    let d = tempfile::tempdir().unwrap();
    assert!(Schema::load(Some(d.path())).is_err());
    let p = d.path().join("sources.tsv");
    for text in [
        "wrong\n",
        "source\tdescription\nx\tone\nx\ttwo\n",
        "source\tdescription\nx\n",
    ] {
        std::fs::write(&p, text).unwrap();
        assert!(Schema::load(Some(d.path())).is_err());
    }
    std::fs::write(p, "source\tdescription\nz\t日本語\na\t説明\n").unwrap();
    assert_eq!(
        Schema::load(Some(d.path())).unwrap().sources,
        [("z".into(), "日本語".into()), ("a".into(), "説明".into())]
    );
    assert!(Schema::load(None).unwrap().sources.is_empty());
}
fn row(key: &str, mv: &str, role: &str, tier: &str, eval: &str) -> Row {
    Row::from_fields(&[
        ("key", key.into()),
        ("move", mv.into()),
        ("role", role.into()),
        ("tier", tier.into()),
        ("since", "test".into()),
        ("source", "test".into()),
        ("evals", eval.into()),
        ("ply", "1".into()),
        ("note", "日本語".into()),
    ])
}
fn args(dir: &Path) -> ExportArgs {
    ExportArgs {
        master: dir.join("moves.tsv"),
        out: dir.join("book"),
        tier: "wide".into(),
        schema: None,
        reads: vec![],
        min_nodes: 900_000_000.into(),
        draw_pin: 3.into(),
        bad_eval: -150.0,
        strict_bad_eval: false,
        base: None,
        force: false,
    }
}
#[test]
fn outputs_reads_schema_and_strict_bad_eval() {
    use super::super::reply_graph::{HIRATE_KEY, fixture::after};
    let d = tempfile::tempdir().unwrap();
    let root = HIRATE_KEY;
    let child = after(root, &["7g7f", "3c3d"]);
    let rows = [
        row(root, "7g7f", "main", "curated", "-200,-201"),
        row(root, "2g2f", "alt", "wide", ""),
        row(root, "5g5f", "record", "wide", ""),
        row(&child, "2g2f", "main", "wide", "12"),
    ];
    std::fs::write(d.path().join("moves.tsv"), master_table::format_master(&rows)).unwrap();
    let mut a = args(d.path());
    a.strict_bad_eval = true;
    let mut output = Vec::new();
    assert!(run_to(a, &mut output).is_err());
    assert!(String::from_utf8(output).unwrap().contains("bad-eval reachable positions: 1"));
    assert!(files::outputs(&d.path().join("book")).iter().all(|p| !p.exists()));
    run_to(args(d.path()), &mut Vec::new()).unwrap();
    let db = std::fs::read_to_string(d.path().join("book.db")).unwrap();
    assert!(db.starts_with("#YANEURAOU-DB2016 1.00\n"));
    assert!(db.contains("7g7f none 100 0 2\n2g2f none 100 0 1\n5g5f none -30000 0 0\n"));
    let annotation: serde_json::Value =
        serde_json::from_slice(&std::fs::read(d.path().join("book.annot.json")).unwrap()).unwrap();
    assert_eq!(annotation["sources"], serde_json::json!({}));
    assert_eq!(
        annotation["positions"][root]["moves"]["7g7f"]["replies"],
        serde_json::json!(["3c3d"])
    );
    assert_eq!(annotation["positions"][root]["moves"]["7g7f"]["depth_after"], 2);
    assert_eq!(annotation["positions"][root]["moves"]["5g5f"]["depth_after"], 0);
    let digest = format!("{:x}", Sha256::digest(db.as_bytes()));
    assert_eq!(
        std::fs::read_to_string(d.path().join("book.default.explore.txt")).unwrap(),
        format!("# preset: default\n# book: book.db sha256 {digest}\nsfen {root} 7g7f\n")
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("book.random.explore.txt")).unwrap(),
        format!("# preset: random\n# book: book.db sha256 {digest}\nsfen {root} 7g7f 2g2f\n")
    );
    let logs = d.path().join("reads.jsonl");
    std::fs::write(&logs,format!(r#"{{"key":"{root}","ply":1,"nodes":1000000000,"multipv":[{{"move":"2g2f","cp":12}},{{"move":"7g7f","cp":10}}]}}"#)).unwrap();
    let mut a = args(d.path());
    a.force = true;
    a.reads = vec![logs];
    a.strict_bad_eval = true;
    run_to(a, &mut Vec::new()).unwrap();
    let text = std::fs::read_to_string(d.path().join("book.annot.json")).unwrap();
    assert!(text.contains("\"top\": 12.0"));
    assert!(text.contains("\"main\": 10.0"));
    std::fs::write(d.path().join("sources.tsv"), "source\tdescription\nunknown\t説明\n").unwrap();
    let mut a = args(d.path());
    a.force = true;
    a.schema = Some(d.path().into());
    assert!(
        run_to(a, &mut Vec::new())
            .unwrap_err()
            .to_string()
            .contains("unknown source: test")
    );
    assert_eq!(std::fs::read_to_string(d.path().join("book.annot.json")).unwrap(), text);
    std::fs::write(d.path().join("sources.tsv"), "source\tdescription\ntest\t説明\n").unwrap();
    let mut a = args(d.path());
    a.force = true;
    a.schema = Some(d.path().into());
    a.tier = "curated".into();
    run_to(a, &mut Vec::new()).unwrap();
    let annotation: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(d.path().join("book.annot.json")).unwrap())
            .unwrap();
    assert_eq!(annotation["sources"], serde_json::json!({"test":"説明"}));
    assert_eq!(annotation["positions"].as_object().unwrap().len(), 1);
    assert_eq!(annotation["positions"][root]["moves"].as_object().unwrap().len(), 1);
}
#[test]
fn validation_reports_all_rows_before_any_output() {
    let d = tempfile::tempdir().unwrap();
    let mut r = row("invalid", "bad", "alt", "wide", "");
    r.set("since", "".into());
    std::fs::write(d.path().join("moves.tsv"), master_table::format_master(&[r])).unwrap();
    let err = run_to(args(d.path()), &mut Vec::new()).unwrap_err().to_string();
    assert!(err.contains("since"));
    assert!(err.contains("main"));
    assert!(err.contains("key"));
    assert!(files::outputs(&d.path().join("book")).iter().all(|p| !p.exists()));
}

#[test]
fn schema_preserves_validation_errors_for_non_utf8_rows() {
    let d = tempfile::tempdir().unwrap();
    let header = master_table::COLUMNS.join("\t");
    std::fs::write(d.path().join("moves.tsv"), [header.as_bytes(), b"\n\xff\nbad\n"].concat())
        .unwrap();
    std::fs::write(d.path().join("sources.tsv"), "source\tdescription\ntest\ttest\n").unwrap();
    let mut a = args(d.path());
    a.schema = Some(d.path().into());
    let error = run_to(a, &mut Vec::new()).unwrap_err().to_string();
    assert!(error.contains("2 行目: UTF-8"));
    assert!(error.contains("3 行目の列の数"));
}

#[test]
fn main_evals_sum_keeps_integer_precision() {
    use super::super::reply_graph::HIRATE_KEY;
    let d = tempfile::tempdir().unwrap();
    let row = row(HIRATE_KEY, "7g7f", "main", "wide", "9007199254740992,-1,-9007199254740992");
    std::fs::write(d.path().join("moves.tsv"), master_table::format_master(&[row])).unwrap();
    let mut a = args(d.path());
    a.bad_eval = -0.1;
    a.strict_bad_eval = true;
    let mut out = Vec::new();
    assert!(run_to(a, &mut out).is_err());
    assert!(String::from_utf8(out).unwrap().contains("eval=-0.333333 basis=evals"));
}

#[test]
fn read_main_at_bad_eval_threshold_is_detected() {
    use super::super::reply_graph::HIRATE_KEY;
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    std::fs::write(
        &a.master,
        master_table::format_master(&[row(HIRATE_KEY, "7g7f", "main", "wide", "")]),
    )
    .unwrap();
    let reads = dir.path().join("reads.jsonl");
    std::fs::write(&reads, format!(r#"{{"key":"{HIRATE_KEY}","nodes":1000000000,"ply":1,"multipv":[{{"move":"7g7f","cp":1.2345678901234567e20}}]}}"#)).unwrap();
    a.reads.push(reads);
    a.bad_eval = 1.2345678901234567e20;
    a.strict_bad_eval = true;
    let mut stdout = Vec::new();
    assert!(run_to(a, &mut stdout).unwrap_err().to_string().contains("strict-bad-eval"));
    let text = String::from_utf8(stdout).unwrap();
    assert!(text.contains("bad-eval reachable positions: 1"), "{text}");
    assert!(text.contains("basis=main"), "{text}");
    assert!(files::outputs(&dir.path().join("book")).iter().all(|p| !p.exists()));
}

#[test]
fn eval_mean_rounds_exact_integer_quotient() {
    use super::super::reply_graph::HIRATE_KEY;
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    std::fs::write(
        &a.master,
        master_table::format_master(&[row(
            HIRATE_KEY,
            "7g7f",
            "main",
            "wide",
            "9007199254740992,1,-9007199254740992",
        )]),
    )
    .unwrap();
    a.bad_eval = 0.3;
    a.strict_bad_eval = true;
    let mut stdout = Vec::new();
    run_to(a, &mut stdout).unwrap();
    assert!(String::from_utf8(stdout).unwrap().contains("bad-eval reachable positions: 0"));
}
