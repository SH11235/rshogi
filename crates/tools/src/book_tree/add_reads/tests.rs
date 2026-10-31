use super::super::reply_graph::HIRATE_KEY;
use super::*;
fn reading(nodes: i64, ply: i64, scores: &[(&str, i64)]) -> Reading {
    Reading {
        nodes: BigRational::from_integer(nodes.into()),
        ply: ply.into(),
        scores: scores
            .iter()
            .map(|(m, v)| (m.to_string(), BigRational::from_integer((*v).into())))
            .collect(),
    }
}
fn group(readings: Vec<Reading>) -> Groups {
    BTreeMap::from([(HIRATE_KEY.into(), readings)])
}
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
#[test]
fn single_shallow_minimum_ply_and_illegal_report() {
    let g = group(vec![
        reading(900_000_000, 1, &[("2g2f", 80)]),
        reading(2_700_000_000, 9, &[("7g7f", 100), ("2g2f", 95), ("bad", 94)]),
    ]);
    let s = select(&[], &g, "cmp").unwrap();
    assert_eq!(s.rows.len(), 1);
    assert_eq!(s.rows[0].get("ply"), "1");
    assert_eq!(
        s.rows[0].get("note"),
        "約 27 億ノードの読み 1 回。1 位 1 回、1 位からの差の平均 0"
    );
    assert_eq!(s.rows[0].get("nodes"), "2.7e9");
    assert!(s.report.iter().all(|r| r[8] == "single" && r[2] == "1" && r[14] == "True"));
    assert_eq!(s.report.iter().find(|r| r[3] == "bad").unwrap()[7], "illegal");
    assert_eq!(s.report.iter().find(|r| r[3] == "2g2f").unwrap()[7], "not_selected");
}
#[test]
fn consensus_roles_and_every_record_reason() {
    let g = group(vec![
        reading(
            1_000_000_000,
            3,
            &[
                ("7g7f", 100),
                ("2g2f", 99),
                ("3g3f", 98),
                ("4g4f", 97),
                ("5g5f", 96),
                ("6g6f", 70),
            ],
        ),
        reading(
            1_000_000_001,
            5,
            &[
                ("2g2f", 100),
                ("7g7f", 99),
                ("3g3f", 98),
                ("6g6f", 70),
                ("5g5f", 56),
            ],
        ),
    ]);
    let s = select(&[], &g, "cmp").unwrap();
    assert_eq!(s.rows[0].get("move"), "2g2f");
    assert_eq!(s.rows[0].get("role"), "main");
    assert_eq!(s.rows.iter().find(|r| r.get("move") == "7g7f").unwrap().get("role"), "alt");
    let note = |mv| s.rows.iter().find(|r| r.get("move") == mv).unwrap().get("note");
    assert!(note("4g4f").contains("2 回のうち 1 回だけ出た"));
    assert!(note("6g6f").contains("比べる範囲の外、差の平均 30 (20 以上)"));
    assert!(note("5g5f").contains("差の平均 24 (20 以上)、差が 40 以上の回あり"));
    assert!(s.report.iter().all(|r| r[8] == "consensus"));
    let s = select(&[row("7g7f", "main")], &g, "cmp").unwrap();
    assert!(
        s.rows
            .iter()
            .all(|r| r.get("role") == "record" && r.get("note").contains("土台の手を優先"))
    );
    assert_eq!(s.report.iter().find(|r| r[3] == "7g7f").unwrap()[7], "existing");
}
#[test]
fn fallback_uses_read_order_and_strict_gap_boundaries() {
    let g = group(vec![
        reading(100, 1, &[("7g7f", 100), ("2g2f", 60)]),
        reading(100, 2, &[("2g2f", 100), ("7g7f", 60)]),
    ]);
    let s = select(&[], &g, "cmp").unwrap();
    assert_eq!(s.rows[0].get("move"), "2g2f");
    assert_eq!(s.rows[0].get("role"), "main");
    assert_eq!(s.rows[1].get("role"), "record");
    assert!(s.report.iter().all(|r| r[8] == "fallback"));
    assert!(s.rows[1].get("note").contains("差の平均 20 (20 以上)、差が 40 以上の回あり"));
}
#[test]
fn numeric_formats_and_quoted_report_fields() {
    for (value, expected) in [
        ("1.234567890125", "1.23456789012"),
        ("0.00001", "1e-05"),
        ("1000000000000", "1e+12"),
        ("999999999999.9", "1e+12"),
        ("0.0001", "0.0001"),
        ("1.5", "1.5"),
        ("-0.0000001", "-1e-07"),
    ] {
        assert_eq!(numbers::display(&numbers::decimal(value).unwrap()).unwrap(), expected);
    }
    assert_eq!(
        numbers::display(&BigRational::new(1.into(), 3.into())).unwrap(),
        "0.333333333333"
    );
    assert_eq!(numbers::node_note(&numbers::decimal("1250000000").unwrap()).unwrap(), "12");
    assert_eq!(numbers::node_note(&numbers::decimal("1350000000").unwrap()).unwrap(), "14");
    let g = group(vec![reading(100, 1, &[("7g7f", 100), ("bad\t\"move", 90)])]);
    let s = select(&[], &g, "cmp").unwrap();
    let bytes = report_bytes(&s.report);
    assert!(String::from_utf8(bytes).unwrap().contains("\"bad\t\"\"move\""));
}
#[test]
fn report_carriage_return_is_quoted_like_python_3_11_and_later_csv() {
    let mut row = std::array::from_fn(|_| String::new());
    row[0] = "a\rb".into();
    let bytes = report_bytes(&[row]);
    assert!(String::from_utf8(bytes).unwrap().contains("\n\"a\rb\"\t"));
}

fn args(dir: &std::path::Path) -> AddReadsArgs {
    AddReadsArgs {
        schema: None,
        master: dir.join("master.tsv"),
        out: dir.join("out.tsv"),
        since: "cmp".into(),
        min_nodes: 0.into(),
        draw_pin: 3.into(),
        logs: vec![dir.join("reads.jsonl")],
    }
}
#[test]
fn writes_both_outputs_and_guards_existing_input_and_invalid_tags() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    let original = master_table::format_master(&[row("7g7f", "main")]);
    std::fs::write(&a.master, &original).unwrap();
    std::fs::write(&a.logs[0], format!(r#"{{"key":"{HIRATE_KEY}","nodes":100,"ply":1,"multipv":[{{"move":"2g2f","cp":10.0}}]}}"#)).unwrap();
    for tag in ["", " ", "x\t", "x\n", "x\r"] {
        let mut a = args(dir.path());
        a.since = tag.into();
        assert!(run_to(a, &mut Vec::new()).is_err());
        assert!(!dir.path().join("out.tsv").exists());
    }
    let report = dir.path().join("out.tsv.report.tsv");
    std::fs::write(&report, "protected").unwrap();
    assert!(run_to(args(dir.path()), &mut Vec::new()).is_err());
    assert!(!dir.path().join("out.tsv").exists());
    assert_eq!(std::fs::read_to_string(&report).unwrap(), "protected");
    std::fs::remove_file(&report).unwrap();
    for input in [&a.master, &a.logs[0]] {
        let mut a = args(dir.path());
        a.out = input.clone();
        assert!(run_to(a, &mut Vec::new()).is_err());
    }
    let mut out = Vec::new();
    run_to(args(dir.path()), &mut out).unwrap();
    let written = std::fs::read(&a.out).unwrap();
    assert!(String::from_utf8(written.clone()).unwrap().contains("\t10\t"));
    assert!(report.is_file());
    assert!(run_to(args(dir.path()), &mut Vec::new()).is_err());
    assert_eq!(std::fs::read(&a.out).unwrap(), written);
    assert_eq!(std::fs::read_to_string(&a.master).unwrap(), original);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("target positions: 1; added moves: 1; new positions (main): 0")
    );
}

#[test]
fn forty_mean_is_not_a_record_and_subforty_single_gap_can_be_chosen() {
    let g = group(vec![
        reading(100, 1, &[("7g7f", 100), ("2g2f", 100), ("3g3f", 60)]),
        reading(101, 1, &[("7g7f", 100), ("2g2f", 61), ("3g3f", 60)]),
    ]);
    let s = select(&[], &g, "cmp").unwrap();
    assert_eq!(s.rows.len(), 2);
    assert_eq!(s.rows.iter().find(|r| r.get("move") == "2g2f").unwrap().get("role"), "alt");
    assert_eq!(s.report.iter().find(|r| r[3] == "3g3f").unwrap()[7], "not_selected");
}
#[test]
fn invalid_master_leaves_both_outputs_absent() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    let mut bad = row("7g7e", "main");
    bad.set("since", String::new());
    std::fs::write(&a.master, master_table::format_master(&[bad])).unwrap();
    std::fs::write(&a.logs[0], "").unwrap();
    let error = run_to(a, &mut Vec::new()).unwrap_err().to_string();
    assert!(error.contains("since") && error.contains("合法手"));
    assert!(!dir.path().join("out.tsv").exists());
    assert!(!dir.path().join("out.tsv.report.tsv").exists());
}

#[test]
fn fractional_cp_errors_without_writing_and_integral_notation_round_trips() {
    for (cp, expected) in [
        ("1.5", None),
        ("15e-1", None),
        ("2e1", Some("20")),
        ("20.0", Some("20")),
        ("1000000000000", None),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let a = args(dir.path());
        let original = master_table::format_master(&[row("2g2f", "main")]);
        std::fs::write(&a.master, &original).unwrap();
        std::fs::write(&a.logs[0], format!(r#"{{"key":"{HIRATE_KEY}","nodes":100,"ply":1,"draw":{{"b":0}},"multipv":[{{"move":"7g7f","cp":{cp}}}]}}"#)).unwrap();
        let mut out = Vec::new();
        let result = run_to(args(dir.path()), &mut out);
        if let Some(expected) = expected {
            result.unwrap();
            let text = std::fs::read_to_string(&a.out).unwrap();
            let rows = master_table::parse_master(&text).unwrap();
            assert_eq!(rows[0].get("evals"), expected);
            master_table::read_master(&a.out).unwrap();
        } else {
            let error = result.unwrap_err().to_string();
            assert!(error.contains(HIRATE_KEY) && error.contains("7g7f"), "{error}");
            assert!(out.is_empty());
            assert!(!a.out.exists());
            assert!(!dir.path().join("out.tsv.report.tsv").exists());
        }
        assert_eq!(std::fs::read_to_string(&a.master).unwrap(), original);
    }
}

#[test]
fn value_kind_counts_each_moves_actual_readings() {
    let g = group(vec![
        reading(1_000_000_000, 1, &[("7g7f", 10), ("2g2f", 0)]),
        reading(1_000_000_000, 2, &[("7g7f", 10)]),
    ]);
    let selected = select(&[], &g, "test").unwrap();
    let main = selected.rows.iter().find(|r| r.get("move") == "7g7f").unwrap();
    let other = selected.rows.iter().find(|r| r.get("move") == "2g2f").unwrap();
    assert_eq!(main.get("value_kind"), "mean:2");
    assert_eq!(other.get("value_kind"), "single");
    for row in selected.rows {
        assert_eq!(row.get("cond"), "unknown");
        assert_eq!(row.get("lineage"), "");
    }
}
