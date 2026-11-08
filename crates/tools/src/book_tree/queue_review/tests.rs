use super::*;
use serde_json::json;
use tempfile::TempDir;

fn args(dir: &Path) -> QueueReviewArgs {
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
        table: dir.join("review.tsv"),
        boxes: dir.join("boxes.tsv"),
        hold_out: dir.join("hold"),
        prefix: "qrhold".into(),
        hold_class: vec![],
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
    a.table = b.join("enqueue/overwrite.jsonl");
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
    fixture_box(&b, rows.iter().map(|r| progress(r, 0, 0)).collect(), vec![]);
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
    fixture_box(&b, rows.iter().map(|r| progress(r, 0, 0)).collect(), vec![]);
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
    std::fs::create_dir_all(&a.hold_out).unwrap();
    let previous = a.hold_out.join("previous_A.jsonl");
    std::fs::write(&previous, "previous\n").unwrap();
    run_at(a, now(), &mut vec![]).unwrap();
    assert_eq!(std::fs::read_to_string(previous).unwrap(), "previous\n");
    assert!(
        std::fs::read_to_string(d.path().join("boxes.tsv"))
            .unwrap()
            .contains("\t92\tinf\t1\n")
    );
}
