use super::*;
use serde_json::json;
use tempfile::TempDir;

fn args(dir: &Path) -> QueueReviewArgs {
    let master = dir.join("master.tsv");
    std::fs::write(&master, super::super::master_table::COLUMNS.join("\t") + "\n").unwrap();
    QueueReviewArgs {
        box_specs: vec![],
        rate: vec![],
        default_rate: 1.44,
        box_rate: vec![],
        cutoff: "2026-11-08T12:00:00+09:00".parse().unwrap(),
        tree_size: 92,
        walk_table: vec![],
        start_moves: None,
        value_floor: 0.001,
        table: Some(dir.join("review.tsv")),
        boxes: Some(dir.join("boxes.tsv")),
        hold_out: Some(dir.join("hold")),
        prefix: "qrhold".into(),
        hold_class: vec![],
        plan_out: None,
        max_ratio: 0.95,
        box_deadlines: false,
        condition: vec![],
        add: vec![],
        update: vec![],
        update_size: 30,
        master: Some(master),
        rerank_table: None,
        rerank_out: None,
        rerank_keep_below: 10,
        group_table: None,
        boost_queue: vec![],
        target_black: 50,
        target_white: 80,
    }
}
use std::path::Path;
fn now() -> DateTime<Utc> {
    "2026-11-08T10:00:00+09:00".parse().unwrap()
}
fn root(moves: &str) -> String {
    format!(
        "sfen {} 1",
        path_keys(&moves.split_whitespace().collect::<Vec<_>>())
            .unwrap()
            .last()
            .unwrap()
    )
}
fn row(id: &str, moves: &str) -> Value {
    json!({"id":id,"root":root(moves),"own":"b","depth":0,"priority":42,"hold":false,
        "memo":format!("walk weight=0.1 moves={moves}; origin=walk"),"opts":{"draw":-70},"game":[]})
}
fn progress(row: &Value, read: u64, requested: u64) -> Value {
    json!({"id":row["id"],"own":row["own"],"priority":row["priority"],"hold":row["hold"],"finished":false,"read":read,"requested":requested})
}
fn fixture_box(dir: &Path, trees: Vec<Value>, workers: Vec<Value>) {
    std::fs::create_dir_all(dir.join("enqueue")).unwrap();
    std::fs::create_dir_all(dir.join("fetch")).unwrap();
    std::fs::write(
        dir.join("fetch/progress.json"),
        json!({"trees":trees,"workers":workers}).to_string(),
    )
    .unwrap();
}
fn enqueue(dir: &Path, file: &str, start: u64, rows: &[Value]) -> u64 {
    let text = rows.iter().map(|r| format!("{r}\n")).collect::<String>();
    std::fs::write(dir.join("enqueue").join(format!("{file}.jsonl")), &text).unwrap();
    let end = start + text.len() as u64;
    std::fs::write(
        dir.join("enqueue").join(format!("{file}.jsonl.receipt")),
        json!({"start":start,"end":end,"lines":rows.len()}).to_string(),
    )
    .unwrap();
    end
}
fn walk(dir: &Path, rows: &str) -> PathBuf {
    let path = dir.join("walk.tsv");
    std::fs::write(&path, format!("side\tstop\tweight\tmoves\n{rows}")).unwrap();
    path
}
fn queued(row: Value) -> Queued {
    let line: Line = serde_json::from_value(row.clone()).unwrap();
    let roots = std::iter::once(&line.root)
        .chain(&line.roots)
        .map(|r| parse_root(r).unwrap().key)
        .collect();
    let primary_root = parse_root(&line.root).unwrap().key;
    Queued {
        primary_root,
        line,
        raw: row,
        roots,
    }
}
fn class(q: &Queued, walk: &Walk) -> (Class, Option<f64>) {
    classify(q, q.roots.first().unwrap(), "b", walk, 0.001, &memo_path(q, &[]))
}

#[test]
fn reach_sums_intermediate_positions_and_out_inherits() {
    let dir = TempDir::new().unwrap();
    let path = walk(
        dir.path(),
        "b\tout\t0.0002\t7g7f 3c3d\nb\town\t0.0003\t7g7f 8c8d\nw\town\t0.9\t7g7f\n",
    );
    let mut a = args(dir.path());
    a.walk_table = vec![format!("b={}", path.display())];
    let w = load_walk(&a, &mut vec![]).unwrap();
    assert_eq!(class(&queued(row("middle", "7g7f")), &w), (Class::ReachPartial, Some(0.0005)));
    assert_eq!(
        class(&queued(row("after", "7g7f 3c3d 2g2f")), &w),
        (Class::BeyondOut, Some(0.0002))
    );
    assert_eq!(class(&queued(row("at", "7g7f 3c3d")), &w), (Class::BeyondOut, Some(0.0002)));
    assert_eq!(class(&queued(row("other", "2g2f")), &w), (Class::NotReached, None));
    a.value_floor = 0.0005;
    let q = queued(row("equal", "7g7f"));
    assert_eq!(
        classify(&q, q.roots.first().unwrap(), "b", &w, a.value_floor, &memo_path(&q, &[])).0,
        Class::ReachHigh
    );
}

#[test]
fn memo_separators_and_mismatched_paths() {
    let w = Walk::default();
    for suffix in ["; note=ok", ",note=ok", "； note", " ; note", " weight=1"] {
        let mut r = row("ok", "7g7f");
        r["memo"] = json!(format!("moves=7g7f{suffix}"));
        assert_eq!(class(&queued(r), &w).0, Class::NotReached);
    }
    for memo in ["moves=2g2f", "moves=7g7f 7g7f", "moves=7g7z", "moves="] {
        let mut r = row("bad", "7g7f");
        r["memo"] = json!(memo);
        assert_eq!(class(&queued(r), &w).0, Class::PathMismatch);
    }
    let mut r = row("seed", "7g7f");
    r["memo"] = json!("game seed");
    assert_eq!(class(&queued(r), &w).0, Class::Seed);
}

#[test]
fn receipt_order_latest_row_duplicates_remaining_and_hold_preservation() {
    let d = TempDir::new().unwrap();
    let a_dir = d.path().join("a");
    let b_dir = d.path().join("b");
    let mut old = row("low", "7g7f");
    old["priority"] = json!(999);
    let low = row("low", "7g7f");
    let seed = {
        let mut r = row("seed", "2g2f");
        r["memo"] = Value::Null;
        r
    };
    let running = row("busy", "7g7f 3c3d");
    let held = {
        let mut r = row("held", "9g9f");
        r["hold"] = json!(true);
        r
    };
    let done = row("done", "1g1f");
    let other = row("other", "7g7f");
    let read = row("read", "2g2f 8c8d");
    let mut done_p = progress(&done, 1, 500);
    done_p["finished"] = json!(true);
    fixture_box(
        &a_dir,
        vec![
            progress(&low, 0, 0),
            progress(&seed, 0, 100),
            progress(&running, 0, 5),
            progress(&held, 0, 1000),
            done_p,
            progress(&other, 0, 0),
            progress(&read, 120, 110),
        ],
        vec![
            json!({"key":parse_root(running["root"].as_str().unwrap()).unwrap().key}),
            json!({"key":null}),
        ],
    );
    let end = enqueue(&a_dir, "z_old", 0, &[old, seed, running, held, done, read]);
    enqueue(&a_dir, "a_new", end, &[low.clone(), other]);
    std::fs::write(a_dir.join("enqueue/unaccepted.jsonl"), "invalid JSON\n").unwrap();
    let dup = row("duplicate", "7g7f");
    fixture_box(&b_dir, vec![progress(&dup, 0, 0)], vec![json!({"key":null})]);
    enqueue(&b_dir, "only", 0, &[dup]);
    let path = walk(d.path(), "b\town\t0.0005\t7g7f\n");
    let mut a = args(d.path());
    a.box_specs = vec![
        format!("A={}", a_dir.display()),
        format!("B={}", b_dir.display()),
    ];
    a.walk_table = vec![format!("b={}", path.display())];
    a.rate = vec!["A=2".into()];
    a.box_rate = vec!["B=10".into()];
    let before = std::fs::read(a_dir.join("enqueue/a_new.jsonl")).unwrap();
    let mut stdout = Vec::new();
    run_at(a, now(), &mut stdout).unwrap();
    let table = std::fs::read_to_string(d.path().join("review.tsv")).unwrap();
    let line = table.lines().find(|l| l.starts_with("A\tlow\t")).unwrap();
    assert!(line.contains("\tb\t42\t"));
    assert!(line.ends_with("A:other,B:duplicate;total=2"));
    assert!(!table.contains("\tbusy\t"));
    assert!(!table.contains("\tread\t"));
    let boxes = std::fs::read_to_string(d.path().join("boxes.tsv")).unwrap();
    assert!(boxes.contains("A\t2\t4\t2\t8\t376\t47\t3\n"), "{boxes}");
    assert!(boxes.contains("B\t1\t10\t2\t20\t92\t4.6\t1\n"));
    let holds = std::fs::read_to_string(d.path().join("hold/qrhold_A.jsonl")).unwrap();
    let rows: Vec<Value> = holds.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.len(), 2);
    let mut expected = low;
    expected["hold"] = json!(true);
    assert_eq!(rows.iter().find(|r| r["id"] == "low").unwrap(), &expected);
    assert_eq!(std::fs::read(a_dir.join("enqueue/a_new.jsonl")).unwrap(), before);
    assert!(String::from_utf8(stdout).unwrap().contains("reach-low: 2"));
}

#[test]
fn start_moves_full_and_relative_paths_agree() {
    let d = TempDir::new().unwrap();
    let path = walk(d.path(), "b\town\t0.2\t7g7f 3c3d\nb\town\t0.3\t8c8d\n");
    let mut a = args(d.path());
    a.start_moves = Some("7g7f".into());
    a.walk_table = vec![format!("b={}", path.display())];
    let w = load_walk(&a, &mut vec![]).unwrap();
    assert!(!w.reach.contains_key(&("b".into(), HIRATE_KEY.into())));
    assert_eq!(w.reach[&("b".into(), parse_root(&root("7g7f")).unwrap().key)], 0.5);
    let mut r = row("relative", "7g7f 3c3d");
    r["memo"] = json!("moves=3c3d;");
    let q = queued(r);
    assert_eq!(
        classify(&q, q.roots.first().unwrap(), "b", &w, 0.001, &memo_path(&q, &["7g7f"])).0,
        Class::ReachHigh
    );
}

#[test]
fn multi_roots_and_bw_do_not_hold_protected_sides() {
    let d = TempDir::new().unwrap();
    let b = d.path().join("b");
    let mut r = row("both", "7g7f");
    r["own"] = json!("bw");
    r["roots"] = json!([root("2g2f")]);
    let mut black = progress(&r, 0, 0);
    black["own"] = json!("b");
    let mut white = black.clone();
    white["own"] = json!("w");
    fixture_box(&b, vec![black, white], vec![]);
    enqueue(&b, "only", 0, &[r]);
    let path = walk(d.path(), "b\town\t0.0005\t7g7f\nw\town\t1\t7g7f\n");
    let mut a = args(d.path());
    a.box_specs = vec![format!("B={}", b.display())];
    a.walk_table = vec![
        format!("b={}", path.display()),
        format!("w={}", path.display()),
    ];
    run_at(a, now(), &mut vec![]).unwrap();
    let table = std::fs::read_to_string(d.path().join("review.tsv")).unwrap();
    assert!(table.contains("reach-low"));
    assert!(table.contains("reach-high"));
    assert!(table.contains("extra-root"));
    assert!(!table.contains("path-mismatch"));
    assert_eq!(std::fs::read_to_string(d.path().join("hold/qrhold_B.jsonl")).unwrap(), "");
    assert!(
        std::fs::read_to_string(d.path().join("boxes.tsv"))
            .unwrap()
            .contains("\tinf\t2\n")
    );
}

#[test]
fn explicit_hold_class_and_output_guards() {
    let d = TempDir::new().unwrap();
    let b = d.path().join("b");
    let r = row("unreached", "7g7f");
    fixture_box(&b, vec![progress(&r, 0, 0)], vec![]);
    enqueue(&b, "only", 0, &[r]);
    let mut a = args(d.path());
    a.box_specs = vec![format!("B={}", b.display())];
    a.hold_class = vec![Class::NotReached];
    a.cutoff = "2026-11-08T00:00:00Z".parse().unwrap();
    run_at(a, now(), &mut vec![]).unwrap();
    assert!(
        !std::fs::read_to_string(d.path().join("hold/qrhold_B.jsonl"))
            .unwrap()
            .is_empty()
    );
    let mut a = args(d.path());
    a.box_specs = vec![format!("B={}", b.display())];
    a.table = Some(b.join("enqueue/overwrite.jsonl"));
    assert!(run_at(a, now(), &mut vec![]).is_err());
    assert!(!b.join("enqueue/overwrite.jsonl").exists());
}

#[test]
fn cli_registers_queue_review_and_defaults() {
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        tree: super::super::TreeArgs,
    }
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "tree",
        "queue-review",
        "--box",
        "A=/path/to/a",
        "--cutoff",
        "2026-11-08T12:00:00+09:00",
        "--table",
        "review.tsv",
        "--boxes",
        "boxes.tsv",
        "--hold-out",
        "holds",
        "--hold-class",
        "not-reached",
        "--hold-class",
        "reach-partial",
        "--hold-class",
        "extra-root",
    ])
    .unwrap();
    let super::super::TreeCommand::QueueReview(args) = cli.tree.command else {
        panic!("queue-review が必要です");
    };
    assert_eq!(args.default_rate, 1.44);
    assert_eq!(args.tree_size, 92);
    assert_eq!(args.value_floor, 0.001);
    assert_eq!(args.prefix, "qrhold");
    assert!(args.plan_out.is_none());
    assert_eq!(args.max_ratio, 0.95);
    assert_eq!(args.update_size, 30);
    assert!(!args.box_deadlines);
    assert_eq!(args.hold_class, [Class::NotReached, Class::ReachPartial, Class::ExtraRoot]);
}

#[test]
fn bw_hold_is_emitted_once_only_when_both_sides_are_unstarted_and_low() {
    for read in [0, 1] {
        let d = TempDir::new().unwrap();
        let b = d.path().join("b");
        let mut r = row("both", "7g7f");
        r["own"] = json!("bw");
        let mut black = progress(&r, 0, 0);
        black["own"] = json!("b");
        let mut white = progress(&r, read, 0);
        white["own"] = json!("w");
        fixture_box(&b, vec![black, white], vec![]);
        enqueue(&b, "only", 0, std::slice::from_ref(&r));
        let path = walk(d.path(), "b\town\t0.0002\t7g7f\nw\town\t0.0003\t7g7f\n");
        let mut a = args(d.path());
        a.box_specs = vec![format!("B={}", b.display())];
        a.walk_table = vec![
            format!("b={}", path.display()),
            format!("w={}", path.display()),
        ];
        run_at(a, now(), &mut vec![]).unwrap();
        let holds = std::fs::read_to_string(d.path().join("hold/qrhold_B.jsonl")).unwrap();
        if read == 0 {
            assert_eq!(holds.lines().count(), 1);
            let actual: Value = serde_json::from_str(holds.trim()).unwrap();
            r["hold"] = json!(true);
            assert_eq!(actual, r);
        } else {
            assert!(holds.is_empty());
        }
    }
}

#[test]
fn inconsistent_snapshots_and_overlapping_receipts_fail_without_output() {
    for overlap in [false, true] {
        let d = TempDir::new().unwrap();
        let b = d.path().join("b");
        let r = row("id", "7g7f");
        let mut p = progress(&r, 0, 0);
        if !overlap {
            p["priority"] = json!(123);
        }
        fixture_box(&b, vec![p], vec![]);
        enqueue(&b, "first", 0, std::slice::from_ref(&r));
        if overlap {
            enqueue(&b, "second", 0, &[r]);
        }
        let mut a = args(d.path());
        a.box_specs = vec![format!("B={}", b.display())];
        assert!(run_at(a, now(), &mut vec![]).is_err());
        assert!(!d.path().join("review.tsv").exists());
    }
}

#[test]
fn merged_walk_paths_keep_intermediate_reach_partial() {
    let d = TempDir::new().unwrap();
    assert_eq!(root("7g7f 3c3d 2g2f 8c8d"), root("2g2f 8c8d 7g7f 3c3d"));
    let path = walk(
        d.path(),
        "b\town\t0.002\t7g7f 3c3d 2g2f 8c8d 2f2e\nb\town\t0.0002\t2g2f 8c8d 9g9f\n",
    );
    let b = d.path().join("b");
    let rows = vec![row("middle", "2g2f 8c8d"), row("stop", "2g2f 8c8d 9g9f")];
    fixture_box(
        &b,
        rows.iter()
            .flat_map(|r| {
                r["own"].as_str().unwrap().chars().map(|own| {
                    let mut tree = progress(r, 0, 0);
                    tree["own"] = json!(own.to_string());
                    tree
                })
            })
            .collect(),
        vec![],
    );
    enqueue(&b, "only", 0, &rows);
    for explicit in [false, true] {
        let mut a = args(d.path());
        a.box_specs = vec![format!("B={}", b.display())];
        a.walk_table = vec![format!("b={}", path.display())];
        if explicit {
            a.hold_class = vec![Class::ReachPartial];
        }
        run_at(a, now(), &mut vec![]).unwrap();
        let table = std::fs::read_to_string(d.path().join("review.tsv")).unwrap();
        assert!(
            table
                .lines()
                .find(|l| l.starts_with("B\tmiddle\t"))
                .unwrap()
                .contains("\treach-partial\t0.0002\t")
        );
        assert!(
            table
                .lines()
                .find(|l| l.starts_with("B\tstop\t"))
                .unwrap()
                .contains("\treach-low\t0.0002\t")
        );
        let hold = std::fs::read_to_string(d.path().join("hold/qrhold_B.jsonl")).unwrap();
        let ids: Vec<String> = hold
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            ids,
            if explicit {
                vec!["middle", "stop"]
            } else {
                vec!["stop"]
            }
        );
    }
}

#[test]
fn memo_moves_requires_a_word_boundary() {
    for memo in ["start_moves=7g7f", "othermoves=7g7f", "_moves=7g7f"] {
        assert!(memo_moves(Some(memo)).is_none());
    }
    assert_eq!(memo_moves(Some("start_moves=2g2f; moves=7g7f")), Some(vec!["7g7f"]));
}

#[test]
fn duplicate_ids_are_bounded_and_deterministic() {
    let d = TempDir::new().unwrap();
    let b = d.path().join("b");
    let rows: Vec<_> = (0..12).map(|i| row(&format!("id{i:02}"), "7g7f")).collect();
    fixture_box(
        &b,
        rows.iter()
            .flat_map(|r| {
                r["own"].as_str().unwrap().chars().map(|own| {
                    let mut tree = progress(r, 0, 0);
                    tree["own"] = json!(own.to_string());
                    tree
                })
            })
            .collect(),
        vec![],
    );
    enqueue(&b, "only", 0, &rows);
    let run = || {
        let mut a = args(d.path());
        a.box_specs = vec![format!("B={}", b.display())];
        run_at(a, now(), &mut vec![]).unwrap();
        std::fs::read_to_string(d.path().join("review.tsv")).unwrap()
    };
    let table = run();
    assert_eq!(table, run());
    for line in table.lines().skip(1) {
        let cells: Vec<_> = line.split('\t').collect();
        let (ids, total) = cells[7].split_once(";total=").unwrap();
        assert_eq!(total, "11");
        let expected: Vec<_> = rows
            .iter()
            .filter_map(|r| {
                let id = r["id"].as_str().unwrap();
                (id != cells[1]).then(|| format!("B:{id}"))
            })
            .take(8)
            .collect();
        assert_eq!(ids, expected.join(","));
    }
}

#[test]
fn previous_hold_files_are_preserved_and_failed_trees_are_counted() {
    let d = TempDir::new().unwrap();
    let b = d.path().join("b");
    let r = row("failed", "7g7f");
    let mut p = progress(&r, 0, 0);
    p["failed"] = json!(true);
    fixture_box(&b, vec![p], vec![]);
    enqueue(&b, "only", 0, &[r]);
    let mut a = args(d.path());
    a.box_specs = vec![format!("B={}", b.display())];
    std::fs::create_dir_all(a.hold_out.as_ref().unwrap()).unwrap();
    let previous = a.hold_out.as_ref().unwrap().join("previous_A.jsonl");
    std::fs::write(&previous, "previous\n").unwrap();
    run_at(a, now(), &mut vec![]).unwrap();
    assert_eq!(std::fs::read_to_string(previous).unwrap(), "previous\n");
    assert!(
        std::fs::read_to_string(d.path().join("boxes.tsv"))
            .unwrap()
            .contains("\t92\tinf\t1\n")
    );
}

#[test]
fn memo_moves_stop_at_trailing_text() {
    let memo = "walk weight=0.003 moves=7g7f 3c3d / 深さ 10 段・相手 2 手に (22 手目まで)";
    assert_eq!(super::memo_moves(Some(memo)).unwrap(), vec!["7g7f", "3c3d"]);
    let memo = "walk weight=0.003 moves=7g7f P*5e 8h2b+ next=x";
    assert_eq!(super::memo_moves(Some(memo)).unwrap(), vec!["7g7f", "P*5e", "8h2b+"]);
}

#[test]
fn stop_that_is_also_passed_through_stays_reach_partial() {
    let d = TempDir::new().unwrap();
    let path = walk(d.path(), "b\town\t0.0002\t7g7f 3c3d\nb\town\t0.0003\t7g7f 3c3d 2g2f\n");
    let b = d.path().join("b");
    let rows = vec![row("both", "7g7f 3c3d")];
    fixture_box(
        &b,
        rows.iter()
            .flat_map(|r| {
                r["own"].as_str().unwrap().chars().map(|own| {
                    let mut tree = progress(r, 0, 0);
                    tree["own"] = json!(own.to_string());
                    tree
                })
            })
            .collect(),
        vec![],
    );
    enqueue(&b, "only", 0, &rows);
    let mut a = args(d.path());
    a.box_specs = vec![format!("B={}", b.display())];
    a.walk_table = vec![format!("b={}", path.display())];
    run_at(a, now(), &mut vec![]).unwrap();
    let table = std::fs::read_to_string(d.path().join("review.tsv")).unwrap();
    assert!(
        table
            .lines()
            .any(|l| l.starts_with("B\tboth\t") && l.contains("\treach-partial\t"))
    );
}

#[test]
fn rare_stop_is_not_final_reach() {
    let d = TempDir::new().unwrap();
    let path = walk(d.path(), "b\trare\t0.0002\t7g7f 3c3d\nb\town\t0.9\t7g7f 8c8d 2g2f\n");
    let b = d.path().join("b");
    let rows = vec![row("rare_root", "7g7f 3c3d")];
    fixture_box(
        &b,
        rows.iter()
            .flat_map(|r| {
                r["own"].as_str().unwrap().chars().map(|own| {
                    let mut tree = progress(r, 0, 0);
                    tree["own"] = json!(own.to_string());
                    tree
                })
            })
            .collect(),
        vec![],
    );
    enqueue(&b, "only", 0, &rows);
    let mut a = args(d.path());
    a.box_specs = vec![format!("B={}", b.display())];
    a.walk_table = vec![format!("b={}", path.display())];
    run_at(a, now(), &mut vec![]).unwrap();
    let table = std::fs::read_to_string(d.path().join("review.tsv")).unwrap();
    assert!(
        table
            .lines()
            .any(|l| l.starts_with("B\trare_root\t") && l.contains("\treach-partial\t"))
    );
}

fn plan_args(dir: &Path, rows: &[(&str, Vec<Value>)]) -> QueueReviewArgs {
    let mut a = args(dir);
    a.plan_out = Some(dir.join("plan"));
    a.tree_size = 10;
    for (name, rows) in rows {
        let b = dir.join(name);
        fixture_box(
            &b,
            rows.iter()
                .flat_map(|r| {
                    r["own"].as_str().unwrap().chars().map(|own| {
                        let mut tree = progress(r, 0, 0);
                        tree["own"] = json!(own.to_string());
                        tree
                    })
                })
                .collect(),
            vec![],
        );
        enqueue(&b, "only", 0, rows);
        a.box_specs.push(format!("{name}={}", b.display()));
        a.box_rate.push(format!("{name}=10"));
    }
    a
}
fn cargo_rows(dir: &Path, name: &str) -> Vec<Value> {
    let path = dir.join(format!("plan/qrhold_{name}.jsonl"));
    if !path.exists() {
        return vec![];
    }
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
fn input_rows(dir: &Path, name: &str, rows: &[Value]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    path
}
fn plan_table(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("plan/qrhold_plan.tsv")).unwrap()
}

#[test]
fn plan_moves_in_priority_order_and_merges_update_without_changing_definition() {
    let d = TempDir::new().unwrap();
    let mut urgent = row("urgent", "7g7f");
    urgent["priority"] = json!(1);
    let mut later = row("later", "2g2f");
    later["priority"] = json!(200);
    let mut a = plan_args(d.path(), &[("B", vec![]), ("A", vec![urgent.clone(), later.clone()])]);
    a.update_size = 2;
    let mut update = later.clone();
    update["memo"] = json!("new memo");
    update["game"] = json!([["8c8d", "7g7f"]]);
    update["priority"] = json!(999);
    a.update = vec![input_rows(d.path(), "update.jsonl", &[update.clone()])];
    let before = std::fs::read(d.path().join("A/enqueue/only.jsonl")).unwrap();
    run_at(a, now(), &mut vec![]).unwrap();
    let source = cargo_rows(d.path(), "A");
    assert_eq!(source.len(), 1);
    assert_eq!(source[0]["hold"], true);
    let destination = cargo_rows(d.path(), "B");
    assert_eq!(destination.len(), 1);
    later["game"] = update["game"].clone();
    later["memo"] = update["memo"].clone();
    assert_eq!(destination[0], later);
    assert!(plan_table(d.path()).contains("move\tlater\tb\t200\tA\tB\t"));
    assert_eq!(std::fs::read(d.path().join("A/enqueue/only.jsonl")).unwrap(), before);
}

#[test]
fn plan_moves_or_holds_requested_remaining_per_side() {
    for own in ["b", "bw"] {
        for can_move in [false, true] {
            let d = TempDir::new().unwrap();
            let mut moving = row("moving", "7g7f");
            moving["own"] = json!(own);
            moving["priority"] = json!(200);
            let mut a =
                plan_args(d.path(), &[("A", vec![moving.clone()]), ("B", vec![]), ("C", vec![])]);
            let trees = own
                .chars()
                .enumerate()
                .map(|(i, side)| {
                    let mut tree = progress(&moving, 0, 30 + i as u64 * 10);
                    tree["own"] = json!(side.to_string());
                    tree
                })
                .collect();
            fixture_box(&d.path().join("A"), trees, vec![]);
            a.box_rate = vec![
                "A=1".into(),
                "B=10".into(),
                format!("C={}", if can_move { 50 } else { 10 }),
            ];
            let mut stdout = Vec::new();
            run_at(a, now(), &mut stdout).unwrap();
            let stdout = String::from_utf8(stdout).unwrap();
            let initial = if own == "b" { "15.000" } else { "35.000" };
            assert!(stdout.contains(&format!("A: 計画 残り/読める量={initial} → 0.000")));
            assert!(stdout.contains("B: 計画 残り/読める量=0.000 → 0.000"));
            assert!(cargo_rows(d.path(), "B").is_empty());
            moving["hold"] = json!(true);
            assert_eq!(cargo_rows(d.path(), "A"), [moving.clone()]);
            if can_move {
                moving["hold"] = json!(false);
                assert_eq!(cargo_rows(d.path(), "C"), [moving]);
                let ratio = if own == "b" { "0.300" } else { "0.700" };
                assert!(stdout.contains(&format!("C: 計画 残り/読める量=0.000 → {ratio}")));
                assert!(plan_table(d.path()).contains("move\tmoving\t"));
            } else {
                assert!(cargo_rows(d.path(), "C").is_empty());
                assert!(plan_table(d.path()).contains("hold-over\tmoving\t"));
            }
        }
    }
}

#[test]
fn plan_move_preserves_same_id_in_destination_receipts() {
    let d = TempDir::new().unwrap();
    let moving = row("same", "7g7f");
    let mut existing = row("same", "2g2f");
    existing["hold"] = json!(true);
    let mut a = plan_args(
        d.path(),
        &[
            ("A", vec![moving.clone()]),
            ("B", vec![existing]),
            ("C", vec![]),
        ],
    );
    a.box_rate = vec!["A=1".into(), "B=100".into(), "C=50".into()];
    run_at(a, now(), &mut vec![]).unwrap();
    assert!(cargo_rows(d.path(), "B").is_empty());
    assert_eq!(cargo_rows(d.path(), "C"), [moving]);
    assert!(plan_table(d.path()).contains("move\tsame\tb\t42\tA\tC\t"));
}

#[test]
fn plan_move_preserves_same_id_already_in_plan() {
    let d = TempDir::new().unwrap();
    let first = row("same", "7g7f");
    let mut second = row("same", "2g2f");
    second["priority"] = json!(200);
    let mut a = plan_args(
        d.path(),
        &[
            ("A", vec![first.clone()]),
            ("B", vec![]),
            ("C", vec![second.clone()]),
        ],
    );
    a.box_rate = vec!["A=1".into(), "B=100".into(), "C=1".into()];
    run_at(a, now(), &mut vec![]).unwrap();
    assert_eq!(cargo_rows(d.path(), "B"), [first]);
    second["hold"] = json!(true);
    assert_eq!(cargo_rows(d.path(), "C"), [second]);
    let table = plan_table(d.path());
    assert!(table.contains("move\tsame\tb\t42\tA\tB\t"));
    assert!(table.contains("hold-over\tsame\tb\t200\tC\tC\t"));
}

#[test]
fn plan_holds_low_before_high_priority_and_keeps_important_overflow() {
    let d = TempDir::new().unwrap();
    let low = row("low", "7g7f");
    let mut high = row("high", "2g2f");
    high["priority"] = json!(200);
    let important = row("important", "9g9f");
    let mut a = plan_args(d.path(), &[("A", vec![low, high, important])]);
    a.box_rate = vec!["A=1".into()];
    let w = walk(d.path(), "b\town\t0.0002\t7g7f\n");
    a.walk_table = vec![format!("b={}", w.display())];
    run_at(a, now(), &mut vec![]).unwrap();
    let table = plan_table(d.path());
    let actions: Vec<_> = table.lines().skip(1).map(|l| l.split('\t').next().unwrap()).collect();
    assert_eq!(actions, ["hold-low", "hold-over", "keep-over"]);
    assert_eq!(cargo_rows(d.path(), "A").len(), 2);
}

#[test]
fn plan_shared_extra_root_blocks_other_condition_but_unique_root_can_move() {
    for shared in [false, true] {
        let d = TempDir::new().unwrap();
        let moving = row("moving", "7g7f");
        let mut peer = row("peer", "2g2f");
        peer["hold"] = json!(true);
        if shared {
            peer["roots"] = json!([root("7g7f")]);
        }
        let mut a = plan_args(d.path(), &[("A", vec![moving, peer]), ("B", vec![])]);
        a.box_rate = vec!["A=1".into(), "B=10".into()];
        a.condition = vec!["A=one".into(), "B=two".into()];
        run_at(a, now(), &mut vec![]).unwrap();
        assert_eq!(cargo_rows(d.path(), "B").len(), usize::from(!shared));
        assert_eq!(plan_table(d.path()).contains("keep-over"), shared);
    }
}

#[test]
fn plan_update_uses_live_owner_or_all_held_copies_and_drops_missing() {
    let d = TempDir::new().unwrap();
    let live = row("live", "7g7f");
    let mut copy = live.clone();
    copy["hold"] = json!(true);
    copy["priority"] = json!(777);
    let mut held = row("held", "2g2f");
    held["hold"] = json!(true);
    let mut a = plan_args(
        d.path(),
        &[
            ("A", vec![live.clone(), held.clone()]),
            ("B", vec![copy, held.clone()]),
        ],
    );
    a.box_rate = vec!["A=100".into(), "B=100".into()];
    let mut update = live;
    update["memo"] = json!("new");
    update["priority"] = json!(123);
    a.update = vec![input_rows(
        d.path(),
        "updates",
        &[update, held, row("missing", "9g9f")],
    )];
    run_at(a, now(), &mut vec![]).unwrap();
    let a_rows = cargo_rows(d.path(), "A");
    assert_eq!(a_rows.len(), 2);
    assert_eq!(a_rows[0]["priority"], 42);
    assert_eq!(a_rows[0]["memo"], "new");
    assert_eq!(cargo_rows(d.path(), "B").len(), 1);
    let table = plan_table(d.path());
    assert_eq!(table.matches("update-held\t").count(), 2);
    assert!(table.contains("drop-update\tmissing"));
}

#[test]
fn plan_adds_by_priority_groups_roots_and_holds_when_full() {
    let d = TempDir::new().unwrap();
    let mut a = plan_args(d.path(), &[("B", vec![]), ("A", vec![])]);
    a.condition = vec!["A=one".into(), "B=two".into()];
    let mut first = row("first", "7g7f");
    first["priority"] = json!(1);
    let second = row("second", "7g7f");
    let third = row("third", "2g2f");
    a.add = vec![input_rows(d.path(), "adds", &[second, third, first])];
    run_at(a, now(), &mut vec![]).unwrap();
    let rows = cargo_rows(d.path(), "A");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "first");
    assert_eq!(rows[0]["hold"], false);
    assert_eq!(rows[1]["id"], "second");
    assert_eq!(rows[1]["hold"], true);
    assert_eq!(cargo_rows(d.path(), "B")[0]["id"], "third");
    assert!(plan_table(d.path()).contains("add-hold\tsecond"));
}

#[test]
fn plan_add_ignores_held_copies_in_receipts_and_updates() {
    for update_held in [false, true] {
        let d = TempDir::new().unwrap();
        let mut copy = row("copy", "2g2f");
        copy["roots"] = json!([root("7g7f")]);
        copy["hold"] = json!(true);
        let live = row("live", "7g7f");
        let mut a = plan_args(d.path(), &[("A", vec![copy.clone()]), ("B", vec![live])]);
        a.box_rate = vec!["A=100".into(), "B=100".into()];
        a.condition = vec!["A=one".into(), "B=two".into()];
        if update_held {
            a.update = vec![input_rows(d.path(), "updates", std::slice::from_ref(&copy))];
        }
        let added = row("new", "7g7f");
        a.add = vec![input_rows(d.path(), "adds", std::slice::from_ref(&added))];
        run_at(a, now(), &mut vec![]).unwrap();
        assert_eq!(cargo_rows(d.path(), "B"), [added]);
        assert_eq!(cargo_rows(d.path(), "A"), if update_held { vec![copy] } else { vec![] });
        assert!(plan_table(d.path()).contains("add\tnew\tb\t42\t\tB\t"));
    }
}

#[test]
fn plan_add_allows_another_condition_for_roots_with_multiple_live_labels() {
    let d = TempDir::new().unwrap();
    let mut a = plan_args(
        d.path(),
        &[
            ("A", vec![row("first", "7g7f")]),
            ("B", vec![row("second", "7g7f")]),
            ("C", vec![]),
        ],
    );
    a.box_rate = vec!["A=100".into(), "B=100".into(), "C=100".into()];
    a.condition = vec!["A=one".into(), "B=two".into(), "C=three".into()];
    let added = row("new", "7g7f");
    a.add = vec![input_rows(d.path(), "adds", std::slice::from_ref(&added))];
    run_at(a, now(), &mut vec![]).unwrap();
    assert_eq!(cargo_rows(d.path(), "C"), [added]);
    assert!(cargo_rows(d.path(), "A").is_empty());
    assert!(cargo_rows(d.path(), "B").is_empty());
    assert!(plan_table(d.path()).contains("add\tnew\tb\t42\t\tC\t"));
}

#[test]
fn plan_add_hold_does_not_constrain_later_adds_to_its_condition() {
    let d = TempDir::new().unwrap();
    let mut a = plan_args(d.path(), &[("A", vec![]), ("B", vec![])]);
    a.box_rate = vec!["A=5".into(), "B=10".into()];
    a.condition = vec!["A=one".into(), "B=two".into()];
    let mut first = row("first", "7g7f");
    first["own"] = json!("bw");
    first["priority"] = json!(1);
    let second = row("second", "7g7f");
    a.add = vec![input_rows(
        d.path(),
        "adds",
        &[first.clone(), second.clone()],
    )];
    run_at(a, now(), &mut vec![]).unwrap();
    first["hold"] = json!(true);
    assert_eq!(cargo_rows(d.path(), "A"), [first]);
    assert_eq!(cargo_rows(d.path(), "B"), [second]);
    let table = plan_table(d.path());
    assert!(table.contains("add-hold\tfirst\tbw\t1\t\tA\t"));
    assert!(table.contains("add\tsecond\tb\t42\t\tB\t"));
}

#[test]
fn box_deadlines_change_capacity_even_without_plan_and_missing_uses_cutoff() {
    let d = TempDir::new().unwrap();
    let mut a = plan_args(d.path(), &[("A", vec![]), ("B", vec![])]);
    a.plan_out = None;
    a.box_deadlines = true;
    std::fs::write(d.path().join("A/deadline"), format!("{}\n", now().timestamp() + 3600)).unwrap();
    run_at(a, now(), &mut vec![]).unwrap();
    let summary = std::fs::read_to_string(d.path().join("boxes.tsv")).unwrap();
    assert!(summary.contains("A\t0\t10\t1\t10\t0\t0\t0"), "{summary}");
    assert!(summary.contains("B\t0\t10\t2\t20\t0\t0\t0"));
}

#[test]
fn plan_is_deterministic_rejects_existing_cargo_and_duplicate_add_ids() {
    let d = TempDir::new().unwrap();
    let r = row("existing", "7g7f");
    let mut a = plan_args(d.path(), &[("B", vec![]), ("A", vec![r.clone()])]);
    let input = input_rows(d.path(), "adds", &[row("new", "2g2f")]);
    a.add = vec![input.clone()];
    run_at(a, now(), &mut vec![]).unwrap();
    let table = plan_table(d.path());
    let b_bytes = std::fs::read(d.path().join("plan/qrhold_B.jsonl")).unwrap();
    let mut a = args(d.path());
    a.box_specs = vec![
        format!("A={}", d.path().join("A").display()),
        format!("B={}", d.path().join("B").display()),
    ];
    a.plan_out = Some(d.path().join("plan"));
    a.tree_size = 10;
    a.box_rate = vec!["A=10".into(), "B=10".into()];
    a.add = vec![input];
    assert!(run_at(a, now(), &mut vec![]).is_err());
    assert_eq!(plan_table(d.path()), table);
    std::fs::remove_dir_all(d.path().join("plan")).unwrap();
    let mut a = args(d.path());
    a.box_specs = vec![
        format!("A={}", d.path().join("A").display()),
        format!("B={}", d.path().join("B").display()),
    ];
    a.plan_out = Some(d.path().join("plan"));
    a.tree_size = 10;
    a.box_rate = vec!["A=10".into(), "B=10".into()];
    a.add = vec![d.path().join("adds")];
    run_at(a, now(), &mut vec![]).unwrap();
    assert_eq!(plan_table(d.path()), table);
    assert_eq!(std::fs::read(d.path().join("plan/qrhold_B.jsonl")).unwrap(), b_bytes);
    std::fs::remove_dir_all(d.path().join("plan")).unwrap();
    let mut a = args(d.path());
    a.box_specs = vec![format!("A={}", d.path().join("A").display())];
    a.plan_out = Some(d.path().join("plan"));
    a.add = vec![input_rows(d.path(), "duplicate", &[r])];
    assert!(run_at(a, now(), &mut vec![]).is_err());
    assert!(!d.path().join("plan").exists());
}

#[test]
fn plan_rejects_multiple_live_update_owners_but_failed_is_not_an_owner() {
    for failed in [false, true] {
        let d = TempDir::new().unwrap();
        let r = row("same", "7g7f");
        let mut a = plan_args(d.path(), &[("A", vec![r.clone()]), ("B", vec![r.clone()])]);
        let mut p = progress(&r, 0, 0);
        p["failed"] = json!(failed);
        fixture_box(&d.path().join("B"), vec![p], vec![]);
        a.box_rate = vec!["A=100".into(), "B=100".into()];
        a.update = vec![input_rows(d.path(), "updates", &[r])];
        assert_eq!(run_at(a, now(), &mut vec![]).is_ok(), failed);
        if failed {
            assert!(cargo_rows(d.path(), "B").is_empty());
        }
    }
}

#[test]
fn plan_does_not_move_read_or_running_trees_and_zero_capacity_add_is_held() {
    let d = TempDir::new().unwrap();
    let read = row("read", "7g7f");
    let running = row("running", "2g2f");
    let mut a = plan_args(d.path(), &[("A", vec![read.clone(), running.clone()]), ("B", vec![])]);
    fixture_box(
        &d.path().join("A"),
        vec![progress(&read, 1, 0), progress(&running, 0, 0)],
        vec![json!({"key":parse_root(running["root"].as_str().unwrap()).unwrap().key})],
    );
    a.box_rate = vec!["A=0".into(), "B=0".into()];
    a.add = vec![input_rows(d.path(), "adds", &[row("new", "9g9f")])];
    let mut stdout = Vec::new();
    run_at(a, now(), &mut stdout).unwrap();
    assert_eq!(cargo_rows(d.path(), "A").len(), 1);
    assert_eq!(cargo_rows(d.path(), "A")[0]["id"], "new");
    assert_eq!(cargo_rows(d.path(), "A")[0]["hold"], true);
    assert!(!d.path().join("plan/qrhold_B.jsonl").exists());
    assert!(String::from_utf8(stdout).unwrap().contains("B: 計画 残り/読める量=inf → inf"));
}

#[test]
fn deadline_uses_earliest_cutoff_and_rejects_invalid_contents() {
    for (offset, expected) in [
        (10_800, Some("\t2\t20\t")),
        (-1, Some("\t0\t0\t")),
        (0, None),
    ] {
        let d = TempDir::new().unwrap();
        let mut a = plan_args(d.path(), &[("A", vec![])]);
        a.box_deadlines = true;
        std::fs::write(
            d.path().join("A/deadline"),
            if expected.is_some() {
                format!("{}\n", now().timestamp() + offset)
            } else {
                "invalid\n".into()
            },
        )
        .unwrap();
        let result = run_at(a, now(), &mut vec![]);
        if let Some(expected) = expected {
            result.unwrap();
            assert!(
                std::fs::read_to_string(d.path().join("boxes.tsv")).unwrap().contains(expected)
            );
        } else {
            assert!(result.is_err());
            assert!(!d.path().join("review.tsv").exists());
        }
    }
}

#[test]
fn plan_output_cannot_share_hold_directory_or_overwrite_an_input() {
    for shared_hold in [false, true] {
        let d = TempDir::new().unwrap();
        let mut a = plan_args(d.path(), &[("A", vec![])]);
        if shared_hold {
            a.plan_out = a.hold_out.clone();
        } else {
            std::fs::create_dir_all(d.path().join("plan")).unwrap();
            let input =
                input_rows(&d.path().join("plan"), "qrhold_plan.tsv", &[row("new", "7g7f")]);
            a.add = vec![input];
        }
        assert!(run_at(a, now(), &mut vec![]).is_err());
        assert!(!d.path().join("review.tsv").exists());
    }
}

#[test]
fn plan_add_uses_only_destination_label_after_cross_condition_move() {
    let d = TempDir::new().unwrap();
    let mut moving = row("moving", "7g7f");
    moving["own"] = json!("bw");
    let mut a = plan_args(d.path(), &[("A", vec![moving]), ("B", vec![])]);
    a.box_rate = vec!["A=7.5".into(), "B=20".into()];
    a.condition = vec!["A=one".into(), "B=two".into()];
    let added = row("new", "7g7f");
    a.add = vec![input_rows(d.path(), "adds", std::slice::from_ref(&added))];
    run_at(a, now(), &mut vec![]).unwrap();
    assert!(plan_table(d.path()).contains("move\tmoving\tbw\t42\tA\tB\t"));
    assert!(cargo_rows(d.path(), "B").contains(&added));
    let source = cargo_rows(d.path(), "A");
    assert_eq!(source.len(), 1);
    assert_eq!(source[0]["hold"], true);
}

#[test]
fn plan_add_ignores_label_of_tree_held_for_overflow() {
    let d = TempDir::new().unwrap();
    let mut held = row("held", "7g7f");
    held["own"] = json!("bw");
    held["priority"] = json!(200);
    let mut a = plan_args(d.path(), &[("A", vec![held]), ("B", vec![])]);
    a.box_rate = vec!["A=1".into(), "B=10".into()];
    a.condition = vec!["A=one".into(), "B=two".into()];
    let added = row("new", "7g7f");
    a.add = vec![input_rows(d.path(), "adds", std::slice::from_ref(&added))];
    run_at(a, now(), &mut vec![]).unwrap();
    assert!(plan_table(d.path()).contains("hold-over\theld\t"));
    assert_eq!(cargo_rows(d.path(), "B"), [added]);
}

#[test]
fn plan_checks_update_game_against_destination_root() {
    let d = TempDir::new().unwrap();
    let original = row("same", "7g7f");
    let mut a = plan_args(d.path(), &[("A", vec![original])]);
    let mut update = row("same", "7g7f 3c3d");
    update["game"] = json!([["2g2f"]]);
    a.update = vec![input_rows(d.path(), "updates", &[update])];
    let error = format!("{:#}", run_at(a, now(), &mut vec![]).unwrap_err());
    assert!(error.contains("台=A id=same"), "{error}");
    assert!(error.contains("game の手が不正"), "{error}");
    assert!(!d.path().join("plan").exists());
}

#[test]
fn plan_move_does_not_inherit_update_sent_only_to_another_box() {
    let d = TempDir::new().unwrap();
    let original = row("same", "7g7f");
    let mut a = plan_args(
        d.path(),
        &[
            ("A", vec![original.clone()]),
            ("B", vec![]),
            ("C", vec![original.clone()]),
        ],
    );
    let mut failed = progress(&original, 0, 0);
    failed["failed"] = json!(true);
    fixture_box(&d.path().join("C"), vec![failed], vec![]);
    a.box_rate = vec!["A=10".into(), "B=10".into(), "C=1".into()];
    a.update_size = 2;
    let mut update = original.clone();
    update["game"] = json!([["8c8d", "2g2f"]]);
    update["memo"] = json!("extra game");
    a.update = vec![input_rows(d.path(), "updates", &[update.clone()])];
    let mut stdout = Vec::new();
    run_at(a, now(), &mut stdout).unwrap();
    let stdout = String::from_utf8(stdout).unwrap();
    assert_eq!(cargo_rows(d.path(), "A"), [update]);
    assert_eq!(cargo_rows(d.path(), "B"), std::slice::from_ref(&original));
    let mut held = original;
    held["hold"] = json!(true);
    assert_eq!(cargo_rows(d.path(), "C"), [held]);
    assert!(plan_table(d.path()).contains("move\tsame\tb\t42\tC\tB\t"));
    assert!(stdout.contains("A: 計画 残り/読める量=0.500 → 0.600"), "{stdout}");
    assert!(stdout.contains("B: 計画 残り/読める量=0.000 → 0.500"), "{stdout}");
    assert!(stdout.contains("C: 計画 残り/読める量=5.000 → 0.000"), "{stdout}");
}

#[test]
fn plan_counts_update_size_once_and_moves_it_with_tree() {
    for moving in [false, true] {
        let d = TempDir::new().unwrap();
        let original = row("same", "7g7f");
        let mut a = plan_args(d.path(), &[("A", vec![original.clone()]), ("B", vec![])]);
        a.box_rate = vec![format!("A={}", if moving { 1 } else { 10 }), "B=10".into()];
        a.update_size = 2;
        let mut update = original.clone();
        update["memo"] = json!("last");
        a.update = vec![input_rows(d.path(), "updates", &[original, update.clone()])];
        let mut stdout = Vec::new();
        run_at(a, now(), &mut stdout).unwrap();
        let stdout = String::from_utf8(stdout).unwrap();
        if moving {
            assert!(stdout.contains("A: 計画 残り/読める量=5.000 → 0.000"), "{stdout}");
            assert!(stdout.contains("B: 計画 残り/読める量=0.000 → 0.600"), "{stdout}");
            assert_eq!(cargo_rows(d.path(), "B"), [update]);
        } else {
            assert!(stdout.contains("A: 計画 残り/読める量=0.500 → 0.600"), "{stdout}");
            assert_eq!(cargo_rows(d.path(), "A"), [update]);
        }
    }
}

#[test]
fn plan_refuses_existing_prefixed_table_even_without_cargo() {
    let d = TempDir::new().unwrap();
    let a = plan_args(d.path(), &[("A", vec![])]);
    std::fs::create_dir_all(d.path().join("plan")).unwrap();
    let path = d.path().join("plan/qrhold_plan.tsv");
    std::fs::write(&path, "existing").unwrap();
    assert!(run_at(a, now(), &mut vec![]).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "existing");
    assert!(!d.path().join("review.tsv").exists());
}

#[test]
fn cli_requires_plan_for_explicit_planning_options_except_deadlines() {
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        tree: super::super::TreeArgs,
    }
    use clap::Parser;
    let base = [
        "tree",
        "queue-review",
        "--box",
        "A=/path/to/a",
        "--cutoff",
        "2026-11-08T12:00:00+09:00",
        "--table",
        "review.tsv",
        "--boxes",
        "boxes.tsv",
        "--hold-out",
        "holds",
    ];
    for option in [
        ["--add", "add.jsonl"],
        ["--update", "update.jsonl"],
        ["--condition", "A=one"],
        ["--max-ratio", "0.95"],
        ["--update-size", "30"],
    ] {
        let mut argv = base.to_vec();
        argv.extend(option);
        let error = Cli::try_parse_from(&argv).err().unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        assert!(error.to_string().contains("--plan-out"));
        argv.extend(["--plan-out", "plan"]);
        assert!(Cli::try_parse_from(argv).is_ok());
    }
    assert!(Cli::try_parse_from(base.into_iter().chain(["--box-deadlines"])).is_ok());
}

#[test]
fn rerank_classes_and_priority_protection() {
    for (reach, failing, boost, floor, expected) in [
        (0.0, 0.005, false, 0.001, 'A'),
        (0.0, 0.0, true, 0.001, 'A'),
        (0.005, 0.004, false, 0.001, 'B'),
        (0.001, 0.0, false, 0.001, 'C'),
        (0.0009, 0.0, false, 0.001, 'D'),
        (0.0009, 0.0, false, 0.0005, 'C'),
    ] {
        assert_eq!(rerank::class(reach, failing, boost, floor), expected);
    }
    let d = TempDir::new().unwrap();
    let mut urgent = row("urgent", "7g7f");
    urgent["priority"] = json!(9);
    urgent["hold"] = json!(true);
    let mut boundary = row("boundary", "7g7f");
    boundary["priority"] = json!(10);
    boundary["root"] = json!(format!(
        "sfen {} 120",
        parse_root(boundary["root"].as_str().unwrap()).unwrap().key
    ));
    let mut a = plan_args(
        d.path(),
        &[(
            "A",
            vec![urgent, boundary, {
                let mut r = row("planned", "7g7f");
                r["priority"] = json!(25);
                r
            }],
        )],
    );
    a.plan_out = None;
    a.table = None;
    a.boxes = None;
    a.hold_out = None;
    a.rerank_table = Some(d.path().join("rerank.tsv"));
    a.rerank_out = Some(d.path().join("rerank"));
    a.walk_table = vec![format!(
        "b={}",
        walk(d.path(), "b\town\t0.005\t7g7f\n").display()
    )];
    let mut stdout = Vec::new();
    run_at(a, now(), &mut stdout).unwrap();
    let text = std::fs::read_to_string(d.path().join("rerank.tsv")).unwrap();
    assert!(text.contains("\tB\t9\t9\ttrue\tfalse\ttrue\t0\t"), "{text}");
    assert!(text.contains("\tB\t25\t25\tfalse\tfalse"), "{text}");
    assert!(text.contains("\tB\t10\t10\tfalse\tfalse\tfalse\t0\t"), "{text}");
    assert!(
        String::from_utf8(stdout)
            .unwrap()
            .contains("priority: 上げる=0 下げる=0 (うち group 揃え=0)、hold: 外す=1 付ける=0")
    );
}

#[test]
fn rerank_shelved_moved_running_and_latest_raw_rows() {
    let d = TempDir::new().unwrap();
    let mut shelved = row("shelved", "7g7f");
    shelved["hold"] = json!(true);
    let mut moved = row("moved", "2g2f");
    moved["hold"] = json!(true);
    let mut destination = moved.clone();
    destination["hold"] = json!(false);
    let mut running_held = shelved.clone();
    running_held["id"] = json!("running-held");
    let running = row("running", "9g9f");
    let done = row("done", "1g1f");
    let failed = row("failed", "3g3f");
    let mut latest = row("latest", "8g8f");
    latest.as_object_mut().unwrap().remove("opts");
    latest.as_object_mut().unwrap().remove("game");
    latest["memo"] = json!("moves=invalid");
    let mut a = plan_args(
        d.path(),
        &[
            (
                "Z",
                vec![
                    shelved.clone(),
                    moved.clone(),
                    running_held.clone(),
                    running.clone(),
                    done.clone(),
                    failed.clone(),
                    latest.clone(),
                ],
            ),
            ("A", vec![shelved.clone(), destination.clone()]),
        ],
    );
    let mut done_p = progress(&done, 0, 0);
    done_p["finished"] = json!(true);
    let mut failed_p = progress(&failed, 0, 0);
    failed_p["failed"] = json!(true);
    fixture_box(
        &d.path().join("Z"),
        vec![
            progress(&shelved, 0, 0),
            progress(&moved, 0, 0),
            progress(&running_held, 1, 0),
            progress(&running, 1, 0),
            done_p,
            failed_p,
            progress(&latest, 0, 0),
        ],
        vec![],
    );
    for entry in std::fs::read_dir(d.path().join("Z/enqueue")).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let mut old = latest.clone();
    old["priority"] = json!(100);
    old["memo"] = json!("old memo");
    let end = enqueue(
        &d.path().join("Z"),
        "z-old",
        0,
        &[
            old,
            shelved.clone(),
            moved,
            running_held,
            running,
            done,
            failed,
        ],
    );
    enqueue(&d.path().join("Z"), "a-new", end, &[latest.clone()]);
    a.plan_out = None;
    a.table = None;
    a.boxes = None;
    a.hold_out = None;
    a.rerank_table = Some(d.path().join("rerank.tsv"));
    a.rerank_out = Some(d.path().join("rerank"));
    a.walk_table = vec![format!(
        "b={}",
        walk(d.path(), "b\town\t0.005\t7g7f\n").display()
    )];
    run_at(a, now(), &mut vec![]).unwrap();
    let text = std::fs::read_to_string(d.path().join("rerank.tsv")).unwrap();
    assert!(text.contains("Z\tshelved\tb\theld"));
    assert!(!text.contains("A\tshelved"));
    assert!(!text.contains("Z\tmoved"));
    assert!(text.contains("A\tmoved\tb\tqueued"));
    assert!(!text.contains("\tdone\t") && !text.contains("\tfailed\t"));
    assert!(text.contains("\tB\t42\t40\ttrue\ttrue\ttrue\t1\t"));
    assert!(text.contains("\tD\t42\t42\tfalse\tfalse\tfalse\t1\t"));
    let rows: Vec<Value> = std::fs::read_to_string(d.path().join("rerank/qrhold_rerank_Z.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    latest["hold"] = json!(true);
    assert!(rows.contains(&latest), "{rows:?}");
    assert!(!d.path().join("review.tsv").exists());
}

#[test]
fn rerank_failing_walk_values_sides_start_and_boost_roots() {
    let d = TempDir::new().unwrap();
    let path = d.path().join("values.tsv");
    std::fs::write(&path, "side\tstop\tweight\tmoves\tentry_value_black\nb\tout\t0.005\t7g7f 3c3d\t49\nb\tout\t0.1\t7g7f 8c8d\t50\nw\tout\t0.006\t7g7f 3c3d\t81\nw\tout\t0.1\t7g7f 8c8d\t80\nw\tout\t0.2\t7g7f 4c4d\t79\nb\town\t0.1\t7g7f\t\n").unwrap();
    let mut a = args(d.path());
    a.rerank_table = Some(d.path().join("rerank.tsv"));
    a.start_moves = Some("7g7f".into());
    a.walk_table = vec![
        format!("b={}", path.display()),
        format!("w={}", path.display()),
    ];
    let w = load_walk(&a, &mut vec![]).unwrap();
    let key = parse_root(&root("7g7f")).unwrap().key;
    assert_eq!(w.failing[&("b".into(), key.clone())], 0.005);
    assert!((w.failing[&("w".into(), key.clone())] - 0.106).abs() < 1e-12);
    let boundary = parse_root(&root("7g7f 8c8d")).unwrap().key;
    assert_eq!(w.failing[&("w".into(), boundary)], 0.1);
    let reached = parse_root(&root("7g7f 4c4d")).unwrap().key;
    assert!(!w.failing.contains_key(&("w".into(), reached)));
    assert!(!w.failing.contains_key(&("b".into(), HIRATE_KEY.into())));
    let old = walk(d.path(), "b\town\t0.1\t7g7f\n");
    a.walk_table = vec![format!("b={}", old.display())];
    assert!(load_walk(&a, &mut vec![]).unwrap().failing.is_empty());

    let mut boost = row("boost", "7g7f");
    boost["roots"] = json!([root("2g2f")]);
    let boost_path = input_rows(d.path(), "boost", &[boost]);
    let mut a = plan_args(d.path(), &[("A", vec![row("primary", "7g7f"), row("extra", "2g2f")])]);
    a.plan_out = None;
    a.rerank_table = Some(d.path().join("rerank.tsv"));
    a.walk_table = vec![format!("b={}", old.display())];
    a.boost_queue = vec![boost_path];
    run_at(a, now(), &mut vec![]).unwrap();
    let text = std::fs::read_to_string(d.path().join("rerank.tsv")).unwrap();
    assert_eq!(text.lines().filter(|l| l.contains("\ttrue\tA\t")).count(), 2);
}

#[test]
fn rerank_bw_scores_each_side_and_preserves_shared_row() {
    for read in [0, 1] {
        let d = TempDir::new().unwrap();
        let mut r = row("both", "7g7f");
        r["own"] = json!("bw");
        r["hold"] = json!(true);
        let mut a = plan_args(d.path(), &[("A", vec![r.clone()])]);
        let mut b = progress(&r, read, 0);
        b["own"] = json!("b");
        let mut w = progress(&r, 0, 0);
        w["own"] = json!("w");
        fixture_box(&d.path().join("A"), vec![b, w], vec![]);
        a.plan_out = None;
        a.table = None;
        a.boxes = None;
        a.hold_out = None;
        a.rerank_table = Some(d.path().join("rerank.tsv"));
        a.rerank_out = Some(d.path().join("rerank"));
        let walk_path = walk(d.path(), "b\town\t0.005\t7g7f\n");
        a.walk_table = vec![
            format!("b={}", walk_path.display()),
            format!("w={}", walk_path.display()),
        ];
        run_at(a, now(), &mut vec![]).unwrap();
        let text = std::fs::read_to_string(d.path().join("rerank.tsv")).unwrap();
        assert!(text.contains("\tb\theld\t1\t0.005\t0.005\t0\t0\tfalse\tB"));
        assert!(text.contains("\tw\theld\t1\t0\t0\t0\t0\tfalse\tD"));
        let emitted: Value = serde_json::from_str(
            &std::fs::read_to_string(d.path().join("rerank/qrhold_rerank_A.jsonl")).unwrap(),
        )
        .unwrap();
        r["priority"] = json!(40);
        r["hold"] = json!(read > 0);
        assert_eq!(emitted, r);
    }
}

#[test]
fn rerank_cli_requires_walk_and_master_but_no_review_outputs() {
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        tree: super::super::TreeArgs,
    }
    use clap::Parser;
    let base = [
        "tree",
        "queue-review",
        "--box",
        "A=/path/to/a",
        "--cutoff",
        "2026-11-08T12:00:00+09:00",
    ];
    for option in [
        ["--rerank-table", "rerank.tsv"],
        ["--rerank-out", "rerank"],
        ["--boost-queue", "seeds.jsonl"],
        ["--rerank-keep-below", "10"],
        ["--target-black", "50"],
        ["--target-white", "80"],
    ] {
        let argv: Vec<_> = base.into_iter().chain(option).collect();
        let error = Cli::try_parse_from(&argv).err().unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        assert!(error.to_string().contains("--walk-table"));
        assert!(
            Cli::try_parse_from(argv.iter().copied().chain(["--walk-table", "b=walk.tsv"]))
                .is_err()
        );
        assert!(
            Cli::try_parse_from(argv.into_iter().chain([
                "--walk-table",
                "b=walk.tsv",
                "--master",
                "moves.tsv"
            ]))
            .is_ok()
        );
    }
    let d = TempDir::new().unwrap();
    let mut a = args(d.path());
    a.rerank_table = Some(d.path().join("rerank.tsv"));
    assert!(run_at(a, now(), &mut vec![]).unwrap_err().to_string().contains("walk-table"));
}

#[test]
fn rerank_branch_effective_and_zero_only_hold() {
    use super::super::master_table;
    for weight in [0.02, 0.0002] {
        let d = TempDir::new().unwrap();
        let mut protected = row("protected", "2g2f");
        protected["priority"] = json!(9);
        let mut a = plan_args(
            d.path(),
            &[("A", vec![row("branch", "7g7f 3c3d"), row("zero", "9g9f"), protected])],
        );
        a.plan_out = None;
        a.table = None;
        a.boxes = None;
        a.hold_out = None;
        a.rerank_table = Some(d.path().join("rerank.tsv"));
        a.rerank_out = Some(d.path().join("rerank"));
        let keys = path_keys(&["7g7f", "3c3d"]).unwrap();
        let rows = vec![
            master_table::Row::from_fields(&[
                ("key", keys[0].clone()),
                ("move", "7g7f".into()),
                ("role", "alt".into()),
            ]),
            master_table::Row::from_fields(&[
                ("key", keys[1].clone()),
                ("move", "3c3d".into()),
                ("role", "record".into()),
            ]),
        ];
        std::fs::write(a.master.as_ref().unwrap(), master_table::format_master(&rows)).unwrap();
        a.walk_table = vec![format!(
            "b={}",
            walk(d.path(), &format!("b\town\t{weight}\t\n")).display()
        )];
        let mut stdout = vec![];
        run_at(a, now(), &mut stdout).unwrap();
        let text = std::fs::read_to_string(d.path().join("rerank.tsv")).unwrap();
        let values: Vec<_> = text
            .lines()
            .find(|l| l.starts_with("A\tbranch\t"))
            .unwrap()
            .split('\t')
            .collect();
        assert_eq!(values[5], "0");
        assert_eq!(values[6].parse::<f64>().unwrap(), weight * 0.5);
        assert_eq!(values[7], "2");
        assert_eq!(values[10], if weight > 0.001 { "B" } else { "D" });
        assert_eq!(values[14], "false");
        assert!(text.contains("\tD\t9\t9\tfalse\tfalse"));
        assert!(text.contains("\tD\t42\t42\tfalse\ttrue"));
        assert!(String::from_utf8(stdout).unwrap().contains("下げる=0"));
    }
}

#[test]
fn rerank_completed_destination_excludes_only_matching_side() {
    for status in ["finished", "failed"] {
        for own in ["b", "bw"] {
            let d = TempDir::new().unwrap();
            let mut source = row("moved", "7g7f");
            source["own"] = json!(own);
            source["hold"] = json!(true);
            let mut destination = source.clone();
            destination["hold"] = json!(false);
            let mut a =
                plan_args(d.path(), &[("A", vec![source]), ("B", vec![destination.clone()])]);
            let mut done = progress(&destination, 0, 0);
            done["own"] = json!("b");
            done[status] = json!(true);
            let mut trees = vec![done];
            if own == "bw" {
                let mut white = progress(&destination, 0, 0);
                white["own"] = json!("w");
                trees.push(white);
            }
            fixture_box(&d.path().join("B"), trees, vec![]);
            a.plan_out = None;
            a.table = None;
            a.boxes = None;
            a.hold_out = None;
            a.rerank_table = Some(d.path().join("rerank.tsv"));
            a.rerank_out = Some(d.path().join("rerank"));
            let walk_path = walk(d.path(), "b\town\t0.01\t7g7f\nw\town\t0.01\t7g7f\n");
            a.walk_table = vec![
                format!("b={}", walk_path.display()),
                format!("w={}", walk_path.display()),
            ];
            run_at(a, now(), &mut vec![]).unwrap();
            let table = std::fs::read_to_string(d.path().join("rerank.tsv")).unwrap();
            assert!(!table.contains("\tmoved\tb\t"), "{table}");
            assert_eq!(table.lines().count(), if own == "bw" { 2 } else { 1 });
            assert!(
                std::fs::read_to_string(d.path().join("rerank/qrhold_rerank_A.jsonl"))
                    .unwrap()
                    .is_empty()
            );
            let updates =
                std::fs::read_to_string(d.path().join("rerank/qrhold_rerank_B.jsonl")).unwrap();
            if own == "bw" {
                assert!(table.contains("B\tmoved\tw\t"));
                let update: Value = serde_json::from_str(&updates).unwrap();
                assert_eq!(update["priority"], 40);
            } else {
                assert!(updates.is_empty());
            }
        }
    }
}

#[test]
fn rerank_worker_at_any_root_preserves_hold_with_zero_read() {
    for held in [false, true] {
        let d = TempDir::new().unwrap();
        let mut r = row("running", "7g7f");
        r["own"] = json!("bw");
        r["roots"] = json!([root("2g2f")]);
        r["hold"] = json!(held);
        let mut a = plan_args(d.path(), &[("A", vec![r.clone()])]);
        let mut black = progress(&r, 0, 0);
        black["own"] = json!("b");
        let mut white = black.clone();
        white["own"] = json!("w");
        fixture_box(
            &d.path().join("A"),
            vec![black, white],
            vec![json!({"key": root("2g2f").strip_prefix("sfen ").unwrap()})],
        );
        a.plan_out = None;
        a.table = None;
        a.boxes = None;
        a.hold_out = None;
        a.rerank_table = Some(d.path().join("rerank.tsv"));
        a.rerank_out = Some(d.path().join("rerank"));
        let walk_path = walk(d.path(), "b\town\t0.01\t7g7f\n");
        a.walk_table = vec![
            format!("b={}", walk_path.display()),
            format!("w={}", walk_path.display()),
        ];
        run_at(a, now(), &mut vec![]).unwrap();
        let table = std::fs::read_to_string(d.path().join("rerank.tsv")).unwrap();
        for line in table.lines().skip(1) {
            let cells: Vec<_> = line.split('\t').collect();
            assert_eq!(cells[14], held.to_string());
            assert_eq!(cells[3], if held { "held" } else { "running" });
        }
        let updates =
            std::fs::read_to_string(d.path().join("rerank/qrhold_rerank_A.jsonl")).unwrap();
        let update: Value = serde_json::from_str(&updates).unwrap();
        assert_eq!(update["hold"], held);
        assert_eq!(update["priority"], 40);
    }
}

#[test]
fn rerank_cli_rejects_other_update_outputs() {
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        tree: super::super::TreeArgs,
    }
    use clap::Parser;
    for output in ["--hold-out", "--plan-out"] {
        let error = Cli::try_parse_from([
            "tree",
            "queue-review",
            "--box",
            "A=/path/to/a",
            "--cutoff",
            "2026-11-08T12:00:00+09:00",
            "--walk-table",
            "b=walk.tsv",
            "--master",
            "moves.tsv",
            "--rerank-out",
            "rerank",
            output,
            "other",
        ])
        .err()
        .unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        assert!(error.to_string().contains("--rerank-out"));
        assert!(error.to_string().contains(output));
        let d = TempDir::new().unwrap();
        let mut a = args(d.path());
        a.rerank_out = Some(d.path().join("rerank"));
        if output == "--plan-out" {
            a.hold_out = None;
            a.plan_out = Some(d.path().join("plan"));
        }
        let error = run_at(a, now(), &mut vec![]).unwrap_err().to_string();
        assert!(error.contains("併用できません"), "{error}");
    }
}

#[test]
fn rerank_requires_walk_table_for_each_target_side() {
    for missing in ["b", "w"] {
        let d = TempDir::new().unwrap();
        let mut r = row("both", "7g7f");
        r["own"] = json!("bw");
        let mut a = plan_args(d.path(), &[("A", vec![r])]);
        a.plan_out = None;
        a.rerank_table = Some(d.path().join("rerank.tsv"));
        let side = if missing == "b" { "w" } else { "b" };
        let path = walk(d.path(), "");
        a.walk_table = vec![format!("{side}={}", path.display())];
        let error = run_at(a, now(), &mut vec![]).unwrap_err();
        assert!(error.to_string().contains(&format!("--walk-table {missing}=")), "{error:#}");
        assert!(!d.path().join("rerank.tsv").exists());
    }
}

#[test]
fn rerank_aligns_group_to_its_smallest_live_priority_and_group_table_compares() {
    let d = TempDir::new().unwrap();
    let grouped = |id: &str, moves: &str, priority: u32, hold: bool| {
        let mut r = row(id, moves);
        r["priority"] = json!(priority);
        r["hold"] = json!(hold);
        r["group"] = json!("x");
        r
    };
    let mut lone = row("lone", "5g5f");
    lone["priority"] = json!(45);
    let rows = vec![
        grouped("g1", "7g7f", 50, false),
        grouped("g2", "2g2f", 120, false),
        grouped("g3", "6g6f", 5, true),
        lone,
    ];
    let mut a = plan_args(d.path(), &[("A", rows)]);
    a.plan_out = None;
    a.table = None;
    a.boxes = None;
    a.hold_out = None;
    a.group_table = Some(d.path().join("groups.tsv"));
    let mut stdout = Vec::new();
    run_at(a, now(), &mut stdout).unwrap();
    let current = std::fs::read_to_string(d.path().join("groups.tsv")).unwrap();
    assert_eq!(
        current,
        "group\tbasis\ttrees\theld\tboxes\tpriority_min\tpriority_max\tfull\tpart\tnone\tremaining\n\
         x\tcurrent\t2\t1\tA\t50\t120\t1\t0\t1\t20\n"
    );
    let mut a = plan_args(d.path(), &[]);
    a.plan_out = None;
    a.table = None;
    a.boxes = None;
    a.hold_out = None;
    a.tree_size = 10;
    a.box_specs = vec![format!("A={}", d.path().join("A").display())];
    a.box_rate = vec!["A=10".into()];
    a.group_table = Some(d.path().join("groups2.tsv"));
    a.rerank_table = Some(d.path().join("rerank.tsv"));
    a.rerank_out = Some(d.path().join("rerank"));
    a.walk_table = vec![format!(
        "b={}",
        walk(d.path(), "b\town\t0.005\t7g7f\nb\town\t0.0005\t2g2f\nb\town\t0.0005\t5g5f\n")
            .display()
    )];
    let mut stdout = Vec::new();
    run_at(a, now(), &mut stdout).unwrap();
    let stdout = String::from_utf8(stdout).unwrap();
    assert!(stdout.contains("上げる=2 下げる=0 (うち group 揃え=1)"), "{stdout}");
    assert!(
        stdout.contains("group (current): 1 組、全部読み切る=0 手が付かない=0 混在=1 全部 hold=0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("group (rerank): 1 組、全部読み切る=1 手が付かない=0 混在=0 全部 hold=0"),
        "{stdout}"
    );
    let table = std::fs::read_to_string(d.path().join("groups2.tsv")).unwrap();
    assert!(
        table.ends_with(
            "x\tcurrent\t2\t1\tA\t50\t120\t1\t0\t1\t20\nx\trerank\t2\t1\tA\t40\t40\t2\t0\t0\t20\n"
        ),
        "{table}"
    );
    let rerank = std::fs::read_to_string(d.path().join("rerank.tsv")).unwrap();
    assert!(
        rerank.lines().any(|l| l.starts_with("A\tg2\t")
            && l.contains("\tD\t120\t40\t")
            && l.ends_with("\tx")),
        "{rerank}"
    );
    assert!(
        rerank
            .lines()
            .any(|l| l.starts_with("A\tg3\t") && l.contains("\t5\t5\ttrue\ttrue\t")),
        "{rerank}"
    );
    let out: Vec<Value> = std::fs::read_to_string(d.path().join("rerank/qrhold_rerank_A.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let g2 = out.iter().find(|r| r["id"] == "g2").unwrap();
    assert_eq!((g2["priority"].clone(), g2["group"].clone()), (json!(40), json!("x")));
}

#[test]
fn group_must_be_short_printable_text() {
    for (group, ok) in [
        (json!("x57kf:w:41"), true),
        (json!(""), false),
        (json!("a\tb"), false),
        (json!("a".repeat(129)), false),
    ] {
        let mut r = row("g", "7g7f");
        r["group"] = group;
        let line: Line = serde_json::from_value(r).unwrap();
        assert_eq!(line.check().is_ok(), ok);
    }
}

#[test]
fn group_minimum_uses_final_line_hold_after_bw_and_started_rules() {
    let d = TempDir::new().unwrap();
    let line = |id: &str, moves: &str, own: &str, hold: bool, group: &str| {
        let mut r = row(id, moves);
        r["own"] = json!(own);
        r["priority"] = json!(100);
        r["hold"] = json!(hold);
        r["group"] = json!(group);
        r
    };
    let side = |r: &Value, own: &str, read: u64, finished: bool| {
        let mut t = progress(r, read, 0);
        t["own"] = json!(own);
        t["finished"] = json!(finished);
        t
    };
    // shelved は棚上げで片側が読み途中、finished は片側が読み終わった bw の行。
    let shelved = line("shelved", "7g7f", "bw", true, "x");
    let live = line("live", "2g2f", "b", false, "x");
    let finished = line("finished", "5g5f", "bw", false, "y");
    let urgent = line("urgent", "6g6f", "b", false, "y");
    let b = d.path().join("A");
    fixture_box(
        &b,
        vec![
            side(&shelved, "b", 1, false),
            side(&shelved, "w", 0, false),
            side(&live, "b", 0, false),
            side(&finished, "b", 0, false),
            side(&finished, "w", 5, true),
            side(&urgent, "b", 0, false),
        ],
        vec![],
    );
    enqueue(&b, "only", 0, &[shelved, live, finished, urgent]);
    let mut a = args(d.path());
    a.table = None;
    a.boxes = None;
    a.hold_out = None;
    a.box_specs = vec![format!("A={}", b.display())];
    a.rerank_out = Some(d.path().join("rerank"));
    let black = d.path().join("black.tsv");
    std::fs::write(&black, "side\tstop\tweight\tmoves\nb\town\t0.002\t2g2f\nb\town\t0.005\t6g6f\n")
        .unwrap();
    let white = d.path().join("white.tsv");
    std::fs::write(&white, "side\tstop\tweight\tmoves\nw\town\t0.005\t7g7f\n").unwrap();
    a.walk_table = vec![
        format!("b={}", black.display()),
        format!("w={}", white.display()),
    ];
    run_at(a, now(), &mut vec![]).unwrap();
    let out: BTreeMap<String, (Value, Value)> =
        std::fs::read_to_string(d.path().join("rerank/qrhold_rerank_A.jsonl"))
            .unwrap()
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                (v["id"].as_str().unwrap().to_owned(), (v["priority"].clone(), v["hold"].clone()))
            })
            .collect();
    assert_eq!(out.get("shelved"), Some(&(json!(40), json!(true))), "{out:?}");
    assert_eq!(out.get("live"), None, "{out:?}");
    assert_eq!(out.get("finished"), Some(&(json!(40), json!(false))), "{out:?}");
    assert_eq!(out.get("urgent"), Some(&(json!(40), json!(false))), "{out:?}");
}

#[test]
fn rerank_scores_added_trees_with_the_same_measure_and_group() {
    let d = TempDir::new().unwrap();
    let mut existing = row("existing", "7g7f");
    existing["priority"] = json!(100);
    existing["group"] = json!("x");
    let mut a = plan_args(d.path(), &[("A", vec![existing])]);
    a.plan_out = None;
    a.table = None;
    a.boxes = None;
    a.hold_out = None;
    a.rerank_table = Some(d.path().join("rerank.tsv"));
    a.rerank_out = Some(d.path().join("rerank"));
    a.walk_table = vec![format!(
        "b={}",
        walk(d.path(), "b\town\t0.002\t7g7f\nb\town\t0.005\t2g2f\n").display()
    )];
    let mut urgent = row("urgent", "2g2f");
    urgent["priority"] = json!(100);
    urgent["group"] = json!("x");
    let mut unknown = row("unknown", "5g5f");
    unknown["priority"] = json!(100);
    a.add = vec![input_rows(
        d.path(),
        "add.jsonl",
        &[urgent.clone(), unknown],
    )];
    run_at(a, now(), &mut vec![]).unwrap();
    let table = std::fs::read_to_string(d.path().join("rerank.tsv")).unwrap();
    assert!(
        table
            .lines()
            .any(|l| l.starts_with("\turgent\tb\tnew\t") && l.contains("\tB\t100\t40\t")),
        "{table}"
    );
    assert!(
        table
            .lines()
            .any(|l| l.starts_with("A\texisting\t") && l.contains("\tC\t100\t40\t")),
        "{table}"
    );
    let read = |name: &str| -> Vec<Value> {
        std::fs::read_to_string(d.path().join("rerank").join(name))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    let added = read("qrhold_rerank-add.jsonl");
    assert_eq!(
        added
            .iter()
            .map(|r| (r["id"].clone(), r["priority"].clone(), r["hold"].clone()))
            .collect::<Vec<_>>(),
        vec![
            (json!("unknown"), json!(100), json!(true)),
            (json!("urgent"), json!(40), json!(false))
        ]
    );
    assert_eq!(read("qrhold_rerank_A.jsonl")[0]["priority"], json!(40));
}

#[test]
fn rerank_add_rejects_accepted_or_duplicate_ids() {
    let d = TempDir::new().unwrap();
    let mut a = plan_args(d.path(), &[("A", vec![row("same", "7g7f")])]);
    a.plan_out = None;
    a.table = None;
    a.boxes = None;
    a.hold_out = None;
    a.rerank_table = Some(d.path().join("rerank.tsv"));
    a.walk_table = vec![format!(
        "b={}",
        walk(d.path(), "b\town\t0.002\t7g7f\n").display()
    )];
    a.add = vec![input_rows(d.path(), "add.jsonl", &[row("same", "2g2f")])];
    let error = run_at(a, now(), &mut vec![]).unwrap_err().to_string();
    assert!(error.contains("受付済み"), "{error}");
}

#[test]
fn cli_add_needs_plan_or_rerank_output() {
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        tree: super::super::TreeArgs,
    }
    use clap::Parser;
    let base = [
        "tree",
        "queue-review",
        "--box",
        "A=/path/to/a",
        "--cutoff",
        "2026-11-08T12:00:00+09:00",
        "--add",
        "add.jsonl",
        "--walk-table",
        "b=walk.tsv",
        "--master",
        "moves.tsv",
    ];
    assert!(Cli::try_parse_from(base).is_err());
    for option in [
        ["--rerank-table", "r.tsv"],
        ["--rerank-out", "r"],
        ["--plan-out", "p"],
    ] {
        assert!(Cli::try_parse_from(base.into_iter().chain(option)).is_ok());
    }
}
