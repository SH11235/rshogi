use super::*;
use std::path::Path;
#[test]
fn schema_is_explicit_and_ordered() {
    let d = tempfile::tempdir().unwrap();
    crate::book_tree::schema::test_schema(d.path());
    assert!(Schema::load(Some(d.path())).is_err());
    let p = d.path().join("sources.tsv");
    for text in [
        "wrong\n",
        "source\tdescription\treplaceable\nx\tone\t0\nx\ttwo\t0\n",
        "source\tdescription\treplaceable\nx\n",
    ] {
        std::fs::write(&p, text).unwrap();
        assert!(Schema::load(Some(d.path())).is_err());
    }
    std::fs::write(p, "source\tdescription\treplaceable\nz\t日本語\t0\na\t説明\t0\n").unwrap();
    assert_eq!(
        Schema::load(Some(d.path())).unwrap().sources,
        [("z".into(), "日本語".into()), ("a".into(), "説明".into())]
    );
    assert!(Schema::load(None).unwrap().sources.is_empty());
}
fn row(key: &str, mv: &str, role: &str, tier: &str, eval: &str) -> Row {
    crate::book_tree::master_table::test_row(&[
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
        journal: vec![],
        min_nodes: 900_000_000.into(),
        draw_pin: 3.into(),
        bad_eval: -150.0,
        strict_bad_eval: false,
        base: None,
        force: false,
        threads: 1,
    }
}
#[test]
fn outputs_reads_schema_and_strict_bad_eval() {
    for unplayed in ["record", "bench"] {
        use super::super::reply_graph::{HIRATE_KEY, fixture::after};
        let d = tempfile::tempdir().unwrap();
        crate::book_tree::schema::test_schema(d.path());
        let root = HIRATE_KEY;
        let child = after(root, &["7g7f", "3c3d"]);
        let rows = [
            row(root, "7g7f", "main", "curated", "-200,-201"),
            row(root, "2g2f", "alt", "wide", ""),
            row(root, "5g5f", unplayed, "wide", ""),
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
            serde_json::from_slice(&std::fs::read(d.path().join("book.annot.json")).unwrap())
                .unwrap();
        assert_eq!(annotation["sources"], serde_json::json!({}));
        assert_eq!(annotation["version"], 2);
        assert_eq!(annotation["positions"][root]["moves"]["7g7f"]["cond"], "unknown");
        assert_eq!(annotation["positions"][root]["moves"]["7g7f"]["value_kind"], "unknown");
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
        std::fs::write(
            d.path().join("sources.tsv"),
            "source\tdescription\treplaceable\nunknown\t説明\t0\n",
        )
        .unwrap();
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
        std::fs::write(
            d.path().join("sources.tsv"),
            "source\tdescription\treplaceable\ntest\t説明\t0\n",
        )
        .unwrap();
        let mut a = args(d.path());
        a.force = true;
        a.schema = Some(d.path().into());
        a.tier = "curated".into();
        run_to(a, &mut Vec::new()).unwrap();
        let annotation: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(d.path().join("book.annot.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(annotation["sources"], serde_json::json!({"test":"説明"}));
        assert_eq!(annotation["positions"].as_object().unwrap().len(), 1);
        assert_eq!(annotation["positions"][root]["moves"].as_object().unwrap().len(), 1);
    }
}
#[test]
fn validation_reports_all_rows_before_any_output() {
    let d = tempfile::tempdir().unwrap();
    crate::book_tree::schema::test_schema(d.path());
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
    crate::book_tree::schema::test_schema(d.path());
    let header = master_table::COLUMNS.join("\t");
    std::fs::write(d.path().join("moves.tsv"), [header.as_bytes(), b"\n\xff\nbad\n"].concat())
        .unwrap();
    std::fs::write(
        d.path().join("sources.tsv"),
        "source\tdescription\treplaceable\ntest\ttest\t0\n",
    )
    .unwrap();
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
    crate::book_tree::schema::test_schema(d.path());
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

#[test]
fn reads_outside_tier_are_discarded_without_changing_outputs() {
    use super::super::reply_graph::{HIRATE_KEY, fixture::after};
    let dir = tempfile::tempdir().unwrap();
    let child = after(HIRATE_KEY, &["7g7f", "3c3d"]);
    let absent = after(HIRATE_KEY, &["2g2f", "8c8d"]);
    let mut a = args(dir.path());
    std::fs::write(
        &a.master,
        master_table::format_master(&[
            row(HIRATE_KEY, "7g7f", "main", "curated", ""),
            row(&child, "2g2f", "main", "wide", ""),
        ]),
    )
    .unwrap();
    let event = |key: &str, draw: &str| {
        format!(
            r#"{{"key":"{key}","nodes":1000000000,"ply":1,"multipv":[{{"move":"7g7f","cp":12}}],"draw":{draw}}}"#
        )
    };
    let logs = dir.path().join("reads.jsonl");
    let root = event(HIRATE_KEY, "null");
    std::fs::write(&logs, &root).unwrap();
    a.tier = "curated".into();
    a.reads = vec![logs.clone()];
    let mut expected_stdout = Vec::new();
    run_to(a, &mut expected_stdout).unwrap();
    let paths = files::outputs(&dir.path().join("book"));
    let expected: Vec<_> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let child_event = event(&child, "null").replace("7g7f", "2g2f");
    std::fs::write(
        &logs,
        [
            root,
            child_event.clone(),
            child_event,
            event(&absent, "null"),
            event(&absent, r#"{"b":0}"#),
            event(&absent, "null"),
        ]
        .join("\n"),
    )
    .unwrap();
    let keys = BTreeSet::from([HIRATE_KEY]);
    let (selected, counts) = reads::load(std::slice::from_ref(&logs), 0, 0, Some(&keys)).unwrap();
    let (_, all_counts) = reads::load(std::slice::from_ref(&logs), 0, 0, None).unwrap();
    assert_eq!((counts.used, counts.duplicates, counts.draw_unset_used), (1, 0, 1));
    assert_eq!((all_counts.used, all_counts.duplicates, all_counts.draw_unset_used), (3, 3, 2));
    assert_eq!(selected.keys().map(String::as_str).collect::<Vec<_>>(), [HIRATE_KEY]);
    let mut a = args(dir.path());
    a.tier = "curated".into();
    a.reads = vec![logs];
    a.force = true;
    let mut stdout = Vec::new();
    run_to(a, &mut stdout).unwrap();
    assert_eq!(stdout, expected_stdout);
    for (path, expected) in paths.iter().zip(expected) {
        assert_eq!(std::fs::read(path).unwrap(), expected);
    }
}

fn journal_record(depth: u32) -> serde_json::Value {
    use crate::book_tree::journal::Settings;
    use crate::book_tree::reply_graph::HIRATE_KEY;
    let version = "book-tree-v1/newgame-ready/node-calib-last-multipv-v1";
    let mut settings: Settings = serde_json::from_value(serde_json::json!({
        "fingerprint": "", "version": version, "engine_sha256": "engine-hash",
        "options": {}, "content_sha256": {}, "nodes": 1000, "multipv": 2, "hash": 256
    }))
    .unwrap();
    settings.fingerprint = settings.computed_fingerprint().unwrap();
    serde_json::json!({
        "kind": version, "provenance": "engine", "settings": settings,
        "reading": {"tag": "test", "ply": 1, "key": HIRATE_KEY, "nodes_limit": 1000,
            "nodes": 1000, "sec": 1.0, "threads": 1, "hash": 256, "draw": {},
            "multipv": [{"move": "7g7f", "cp": 10, "depth": depth}], "bestmove": "7g7f"}
    })
}

#[test]
fn journal_depths_match_conditions_and_preserve_db() {
    use crate::book_tree::reply_graph::HIRATE_KEY;
    let d = tempfile::tempdir().unwrap();
    crate::book_tree::schema::test_schema(d.path());
    std::fs::write(
        d.path().join("sources.tsv"),
        "source\tdescription\treplaceable\ntest\t説明\t0\n",
    )
    .unwrap();
    let conditions = d.path().join("conditions.tsv");
    let mut table = std::fs::read_to_string(&conditions).unwrap();
    let fp = journal_record(30)["settings"]["fingerprint"].as_str().unwrap().to_owned();
    let uppercase_fp = fp.to_ascii_uppercase();
    let values = [
        uppercase_fp.as_str(),
        "nodes:1000",
        "engine",
        "1",
        "256",
        "2",
        "0",
        "0",
        "engine-hash",
        "評価関数",
        "計算機",
        "条件の説明",
    ];
    table.push_str(&format!("named\t{}\n", values.join("\t")));
    std::fs::write(conditions, table).unwrap();
    let mut rows = vec![
        row(HIRATE_KEY, "7g7f", "main", "wide", "10"),
        row(HIRATE_KEY, "2g2f", "alt", "wide", "10"),
        row(HIRATE_KEY, "5g5f", "record", "wide", "1"),
    ];
    rows[0].set("cond", "named".into());
    rows[1].set("cond", "named".into());
    std::fs::write(d.path().join("moves.tsv"), master_table::format_master(&rows)).unwrap();
    let mut a = args(d.path());
    a.schema = Some(d.path().into());
    run_to(a, &mut Vec::new()).unwrap();
    let preserved: Vec<_> = files::outputs(&d.path().join("book"))
        .iter()
        .take(3)
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    let db = preserved[0].clone();
    let annotation = || -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(d.path().join("book.annot.json")).unwrap()).unwrap()
    };
    let before = annotation();
    assert!(before["positions"][HIRATE_KEY]["moves"]["7g7f"].get("depths").is_none());
    let expected: serde_json::Map<_, _> = [
        "fingerprint",
        "stop",
        "nodes_counting",
        "threads",
        "hash_mb",
        "multipv",
        "draw_black",
        "draw_white",
        "engine_sha256",
        "eval",
        "machine",
        "note",
    ]
    .into_iter()
    .zip(values)
    .map(|(k, v)| (k.into(), serde_json::json!(v)))
    .collect();
    let mut expected = expected;
    expected.insert("fingerprint".into(), serde_json::json!(fp));
    assert_eq!(before["conditions"], serde_json::json!({"named": expected}));
    let logs = d.path().join("journals");
    std::fs::create_dir_all(logs.join("nested")).unwrap();
    std::fs::write(logs.join("journal_extend_r1.jsonl"), "{\"kind\":\"parent\"}\n{invalid}\n")
        .unwrap();
    std::fs::write(logs.join("targets.jsonl"), "{\"id\":\"target\"}\n").unwrap();
    std::fs::write(logs.join("empty.jsonl"), "").unwrap();
    let first = journal_record(30);
    let mut copy = first.clone();
    copy["reading"]["key"] = serde_json::json!(format!("{HIRATE_KEY} 99"));
    copy["reading"]["nodes"] = serde_json::json!(900);
    let mut other = journal_record(90);
    other["provenance"] = serde_json::json!("fixture-import");
    let mut foreign = journal_record(80);
    let mut settings: crate::book_tree::journal::Settings =
        serde_json::from_value(foreign["settings"].clone()).unwrap();
    settings.engine_sha256 = "other-engine".into();
    settings.fingerprint = settings.computed_fingerprint().unwrap();
    foreign["settings"] = serde_json::to_value(settings).unwrap();
    std::fs::write(logs.join("one.jsonl"), format!("{first}\n{foreign}\n")).unwrap();
    let second = logs.join("nested/two.jsonl");
    std::fs::write(&second, format!("{copy}\n{}\n{other}\n{{unfinished", journal_record(20)))
        .unwrap();
    let journal_before = std::fs::read(&second).unwrap();
    let mut a = args(d.path());
    a.schema = Some(d.path().into());
    a.journal = vec![logs.clone(), logs.join("one.jsonl")];
    a.force = true;
    run_to(a, &mut Vec::new()).unwrap();
    let after = annotation();
    let moves = &after["positions"][HIRATE_KEY]["moves"];
    assert_eq!(moves["7g7f"]["depths"], serde_json::json!([30]));
    assert_eq!(moves["2g2f"]["depths"], serde_json::json!([]));
    assert!(moves["5g5f"].get("depths").is_none());
    assert_eq!(after["conditions"], before["conditions"]);
    let mut without_depths = after.clone();
    for position in without_depths["positions"].as_object_mut().unwrap().values_mut() {
        for mv in position["moves"].as_object_mut().unwrap().values_mut() {
            mv.as_object_mut().unwrap().remove("depths");
        }
    }
    assert_eq!(without_depths, before);
    for (path, bytes) in files::outputs(&d.path().join("book")).iter().zip(&preserved) {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
    assert_eq!(after["version"], 2);
    assert_eq!(std::fs::read(d.path().join("book.db")).unwrap(), db);
    assert_eq!(std::fs::read(&second).unwrap(), journal_before);

    rows[0].set("cond", fp[..8].into());
    rows[1].set("cond", fp[..8].into());
    std::fs::write(d.path().join("moves.tsv"), master_table::format_master(&rows)).unwrap();
    let mut a = args(d.path());
    a.journal = vec![logs.clone()];
    a.force = true;
    run_to(a, &mut Vec::new()).unwrap();
    assert_eq!(
        annotation()["positions"][HIRATE_KEY]["moves"]["7g7f"]["depths"],
        serde_json::json!([30])
    );
    assert_eq!(annotation()["conditions"], serde_json::json!({}));

    std::fs::write(&second, format!("{first}\n{{invalid}}\n")).unwrap();
    let mut a = args(d.path());
    a.journal = vec![logs.clone()];
    a.force = true;
    assert!(run_to(a, &mut Vec::new()).is_err());
    assert_eq!(std::fs::read(d.path().join("book.db")).unwrap(), db);

    let mut invalid = first.clone();
    invalid["reading"]["nodes_limit"] = serde_json::json!(0);
    std::fs::write(second, format!("{first}\n{invalid}\n")).unwrap();
    let mut a = args(d.path());
    a.journal = vec![logs];
    a.force = true;
    run_to(a, &mut Vec::new()).unwrap();
    assert_eq!(
        annotation()["positions"][HIRATE_KEY]["moves"]["7g7f"]["depths"],
        serde_json::json!([30])
    );
    assert_eq!(std::fs::read(d.path().join("book.db")).unwrap(), db);
}

#[test]
fn journal_depths_export_imported_readings_once_and_prefer_engine() {
    use crate::book_tree::reply_graph::HIRATE_KEY;
    let dir = tempfile::tempdir().unwrap();
    let engine = journal_record(30);
    let fp = engine["settings"]["fingerprint"].as_str().unwrap();
    let mut imported = engine.clone();
    imported["provenance"] = serde_json::json!(format!(
        "journal:sha256={}:fingerprint={fp}:force=false:path=source.jsonl",
        "a".repeat(64)
    ));
    let mut conflicting = imported.clone();
    conflicting["reading"]["multipv"][0]["depth"] = serde_json::json!(90);
    let mut resumed = imported.clone();
    resumed["provenance"] =
        serde_json::json!(format!("{}:offset=4096", imported["provenance"].as_str().unwrap()));
    let mut other = engine.clone();
    other["provenance"] = serde_json::json!("fixture-import");

    let mut r = row(HIRATE_KEY, "7g7f", "main", "wide", "10");
    r.set("cond", fp[..8].into());
    std::fs::write(dir.path().join("moves.tsv"), master_table::format_master(&[r])).unwrap();
    let journal = dir.path().join("journal.jsonl");
    for (records, expected) in [
        (vec![&imported], Some(30)),
        (vec![&resumed], Some(30)),
        (vec![&imported, &engine], Some(30)),
        (vec![&conflicting, &engine], Some(30)),
        (vec![&engine, &conflicting], Some(30)),
        (vec![&imported, &conflicting], Some(30)),
        (vec![&other], None),
    ] {
        let contents: String = records.iter().map(|record| format!("{record}\n")).collect();
        std::fs::write(&journal, &contents).unwrap();
        let mut a = args(dir.path());
        a.journal = vec![journal.clone()];
        a.force = true;
        run_to(a, &mut Vec::new()).unwrap();
        let annotation: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("book.annot.json")).unwrap())
                .unwrap();
        let depths = annotation["positions"][HIRATE_KEY]["moves"]["7g7f"].get("depths");
        assert_eq!(depths.cloned(), expected.map(|depth| serde_json::json!([depth])), "{contents}");
    }
}

#[test]
fn journal_depths_export_from_original_and_merged_journals() {
    use crate::book_tree::{TreeArgs, reply_graph::HIRATE_KEY};
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        tree: TreeArgs,
    }

    for merged_depth in [30, 90] {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        let model = dir.path().join("model.bin");
        std::fs::write(&engine, "engine").unwrap();
        std::fs::write(&model, "model").unwrap();

        let first_engine = dir.path().join("b-engine.jsonl");
        let second_engine = dir.path().join("c-engine.jsonl");
        let merged = dir.path().join("a-merged.jsonl");
        let conflicts = dir.path().join("conflicts.tsv");
        let first = journal_record(30);
        std::fs::write(&first_engine, format!("{first}\n")).unwrap();
        std::fs::write(&second_engine, format!("{}\n", journal_record(merged_depth))).unwrap();

        let cli = Cli::try_parse_from([
            "tree",
            "import-journal",
            "--engine",
            engine.to_str().unwrap(),
            "--engine-option",
            &format!("EvalFile={}", model.display()),
            "--nodes",
            "1000",
            "--from-journal",
            second_engine.to_str().unwrap(),
            "--from-journal",
            first_engine.to_str().unwrap(),
            "--journal",
            merged.to_str().unwrap(),
            "--on-conflict",
            "keep-existing",
            "--conflicts-out",
            conflicts.to_str().unwrap(),
        ])
        .unwrap();
        crate::book_tree::run(cli.tree).unwrap();
        let conflict_list = std::fs::read_to_string(&conflicts).unwrap();
        assert_eq!(conflict_list.lines().skip(1).count(), usize::from(merged_depth != 30));

        let fp = first["settings"]["fingerprint"].as_str().unwrap();
        let mut r = row(HIRATE_KEY, "7g7f", "main", "wide", "10");
        r.set("cond", fp[..8].into());
        std::fs::write(dir.path().join("moves.tsv"), master_table::format_master(&[r])).unwrap();
        for (journals, expected) in [
            (vec![second_engine.clone(), first_engine.clone()], 30),
            (vec![merged.clone()], merged_depth),
            (vec![merged, second_engine, first_engine], 30),
        ] {
            let mut a = args(dir.path());
            a.journal = journals;
            a.force = true;
            run_to(a, &mut Vec::new()).unwrap();
            let annotation: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.path().join("book.annot.json")).unwrap())
                    .unwrap();
            assert_eq!(
                annotation["positions"][HIRATE_KEY]["moves"]["7g7f"]["depths"],
                serde_json::json!([expected]),
                "merged depth: {merged_depth}",
            );
        }
    }
}

#[test]
fn journal_depths_skip_files_without_a_tree_first_record() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("input.jsonl");
    for contents in [
        "",
        "{unfinished",
        "{\"kind\":\"parent\"}\n{invalid}\n",
        "{invalid}\n",
    ] {
        std::fs::write(&path, contents).unwrap();
        let mut visited = 0;
        let result = crate::book_tree::journal::visit_records(&path, |_| {
            visited += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(result, None);
        assert_eq!(visited, 0);
    }
}

#[test]
fn journal_depths_reject_invalid_tree_records() {
    use crate::book_tree::reply_graph::HIRATE_KEY;
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("input.jsonl");
    let keys = BTreeSet::from([HIRATE_KEY]);
    for case in ["depth", "version"] {
        let mut record = journal_record(30);
        match case {
            "depth" => {
                record["reading"]["multipv"][0].as_object_mut().unwrap().remove("depth");
            }
            "version" => record["kind"] = serde_json::json!("book-tree-unsupported"),
            _ => unreachable!(),
        }
        std::fs::write(&path, format!("{record}\n")).unwrap();
        let error = journal_depths::load(
            std::slice::from_ref(&path),
            &keys,
            &super::super::reply_graph::thread_pool(2).unwrap(),
        )
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains(path.to_str().unwrap()), "{case}: {error:#}");
    }
}

#[test]
fn journal_depths_skip_invalid_rows_and_keep_valid_rows_after_them() {
    use crate::book_tree::reply_graph::{HIRATE_KEY, fixture::after};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let first = journal_record(30);
    let mut invalid = Vec::new();
    let mut tampered = first.clone();
    tampered["settings"]["fingerprint"] = serde_json::json!("a".repeat(64));
    invalid.push(tampered);
    let mut uncomputable = first.clone();
    uncomputable["settings"]["options"]["EvalFile"] = serde_json::json!("missing-hash-model");
    uncomputable["settings"]["content_sha256"] = serde_json::json!({});
    invalid.push(uncomputable);
    let mut inconsistent = first.clone();
    inconsistent["reading"]["threads"] = serde_json::json!(2);
    invalid.push(inconsistent);
    let mut outside = first.clone();
    outside["reading"]["key"] = serde_json::json!(after(HIRATE_KEY, &["7g7f"]));
    outside["reading"]["nodes_limit"] = serde_json::json!(0);
    invalid.push(outside);
    invalid.push(first.clone());
    std::fs::write(&path, invalid.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    let depths = journal_depths::load(
        &[path],
        &BTreeSet::from([HIRATE_KEY]),
        &super::super::reply_graph::thread_pool(2).unwrap(),
    )
    .unwrap();
    let mut r = row(HIRATE_KEY, "7g7f", "main", "wide", "10");
    r.set("cond", first["settings"]["fingerprint"].as_str().unwrap()[..8].into());
    assert_eq!(depths.depths(&Schema::default(), &r).unwrap(), Some(vec![30]));
}

#[test]
fn journal_depths_keep_first_values_and_all_multipv() {
    use crate::book_tree::reply_graph::HIRATE_KEY;
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("input.jsonl");
    let mut first = journal_record(30);
    first["reading"]["multipv"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"move": "2g2f", "cp": 5, "depth": 25}));
    let mut second = first.clone();
    second["reading"]["multipv"][0]["cp"] = serde_json::json!(20);
    std::fs::write(&path, format!("{first}\n{second}\n")).unwrap();
    let depths = journal_depths::load(
        &[path],
        &BTreeSet::from([HIRATE_KEY]),
        &super::super::reply_graph::thread_pool(2).unwrap(),
    )
    .unwrap();
    let schema = Schema::default();
    let mut r = row(HIRATE_KEY, "7g7f", "main", "wide", "10");
    r.set("cond", first["settings"]["fingerprint"].as_str().unwrap()[..8].into());
    assert_eq!(depths.depths(&schema, &r).unwrap(), Some(vec![30]));
    r.set("move", "2g2f".into());
    assert_eq!(depths.depths(&schema, &r).unwrap(), Some(vec![25]));
}

#[test]
fn export_threads_preserve_all_files_and_reports() {
    use crate::book_tree::reply_graph::{HIRATE_KEY, fixture::after};
    let dir = tempfile::tempdir().unwrap();
    let child = after(HIRATE_KEY, &["7g7f", "3c3d"]);
    let mut main = row(HIRATE_KEY, "7g7f", "main", "curated", "-200,-201");
    let first = journal_record(30);
    let fp = first["settings"]["fingerprint"].as_str().unwrap();
    main.set("cond", fp[..8].into());
    let rows = [
        row(&child, "2g2f", "main", "wide", "12"),
        row(HIRATE_KEY, "5g5f", "bench", "wide", ""),
        row(HIRATE_KEY, "2g2f", "alt", "wide", ""),
        main,
    ];
    let mut a = args(dir.path());
    std::fs::write(&a.master, master_table::format_master(&rows)).unwrap();
    let journals = dir.path().join("journals");
    std::fs::create_dir(&journals).unwrap();
    std::fs::write(journals.join("a.jsonl"), format!("{first}\n{}\n", journal_record(20))).unwrap();
    std::fs::write(
        journals.join("b.jsonl"),
        format!("{}\n{first}\n{{unfinished", journal_record(40)),
    )
    .unwrap();
    std::fs::write(journals.join("c.jsonl"), "{\"kind\":\"parent\"}\n").unwrap();
    let mut invalid = journal_record(50);
    invalid["settings"]["fingerprint"] = serde_json::json!("invalid");
    for name in ["d", "e", "f", "g", "h", "i", "j"] {
        std::fs::write(
            journals.join(format!("{name}.jsonl")),
            format!("{invalid}\n{first}\n{}\n", journal_record(60)).repeat(193),
        )
        .unwrap();
    }
    a.journal.push(journals.clone());
    let mut expected_report = Vec::new();
    run_to(a, &mut expected_report).unwrap();
    let paths = files::outputs(&dir.path().join("book"));
    let expected: Vec<_> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let annotation: serde_json::Value = serde_json::from_slice(&expected[3]).unwrap();
    assert_eq!(
        annotation["positions"][HIRATE_KEY]["moves"]["7g7f"]["depths"],
        serde_json::json!([30])
    );
    for threads in [2, 4, 0] {
        let mut a = args(dir.path());
        a.threads = threads;
        a.force = true;
        a.journal.push(journals.clone());
        let mut report = Vec::new();
        run_to(a, &mut report).unwrap();
        assert_eq!(report, expected_report, "threads={threads}");
        for (path, bytes) in paths.iter().zip(&expected) {
            assert_eq!(
                &std::fs::read(path).unwrap(),
                bytes,
                "{}: threads={threads}",
                path.display()
            );
        }
    }
}

#[test]
fn export_parallel_journal_failure_keeps_outputs() {
    use crate::book_tree::reply_graph::HIRATE_KEY;
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    std::fs::write(
        &a.master,
        master_table::format_master(&[row(HIRATE_KEY, "7g7f", "main", "wide", "10")]),
    )
    .unwrap();
    run_to(a, &mut Vec::new()).unwrap();
    let paths = files::outputs(&dir.path().join("book"));
    let expected: Vec<_> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let first = dir.path().join("a.jsonl");
    let second = dir.path().join("b.jsonl");
    let record = journal_record(30);
    std::fs::write(&first, format!("{record}\n{{invalid}}\n")).unwrap();
    std::fs::write(&second, format!("{record}\n").repeat(1000)).unwrap();
    for threads in [1, 2, 0] {
        let mut a = args(dir.path());
        a.force = true;
        a.threads = threads;
        a.journal = vec![first.clone(), second.clone()];
        let error = run_to(a, &mut Vec::new()).unwrap_err();
        // エラーには正規化した台帳のパスが出る (Windows では短い名前の dir が長い名前に変わる)。
        let shown = first.canonicalize().unwrap();
        assert!(format!("{error:#}").contains(shown.to_str().unwrap()));
        for (path, bytes) in paths.iter().zip(&expected) {
            assert_eq!(&std::fs::read(path).unwrap(), bytes);
        }
    }
}

#[test]
fn journal_depths_exclude_marked_shallow_readings() {
    use crate::book_tree::reply_graph::HIRATE_KEY;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut record = journal_record(30);
    record["reading"]["tag"] = serde_json::json!("serve:shallow");
    std::fs::write(&path, format!("{record}\n")).unwrap();
    let depths = journal_depths::load(
        &[path],
        &BTreeSet::from([HIRATE_KEY]),
        &super::super::reply_graph::thread_pool(2).unwrap(),
    )
    .unwrap();
    let mut r = row(HIRATE_KEY, "7g7f", "main", "wide", "10");
    r.set("cond", record["settings"]["fingerprint"].as_str().unwrap()[..8].into());
    assert_eq!(depths.depths(&Schema::default(), &r).unwrap(), None);
}
