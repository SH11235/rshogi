use super::super::master_table::test_row;
use super::super::reply_graph::fixture::{ROOT, after, master_text};
use super::*;
use clap::Parser;
use std::path::Path;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: ExitsArgs,
}

fn args(dir: &Path, own: &str) -> ExitsArgs {
    Cli::try_parse_from([
        "test",
        "--master",
        dir.join("moves.tsv").to_str().unwrap(),
        "--table",
        dir.join("exits.tsv").to_str().unwrap(),
        "--side",
        own,
    ])
    .unwrap()
    .args
}

fn walk(args: &ExitsArgs, initial: &str) -> (usize, Vec<Exit>) {
    let mut index = MasterIndex::open(&args.master, &Schema::load(None).unwrap()).unwrap();
    let mut traversal = traverse(&mut index, args, initial).unwrap();
    let covered = coverage(args).unwrap();
    let mut exits = Vec::new();
    while let Some(mut exit) = traversal.next_exit().unwrap() {
        cover(&mut exit, &covered);
        let mut entries = BTreeMap::new();
        traversal
            .entries(&exit, |entry| {
                entries.insert((entry.key.clone(), entry.mv.clone()), entry.clone());
                Ok(())
            })
            .unwrap();
        exit.entries = entries;
        exits.push(exit);
    }
    (traversal.visited, exits)
}

fn write_master(args: &ExitsArgs, rows: &[(&str, &str, &str, &str)]) {
    std::fs::write(&args.master, master_text(rows)).unwrap();
}

#[test]
fn own_main_opponent_main_and_alt_and_value_sign() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path(), "b");
    let w = after(ROOT, &["5i4h"]);
    let record = after(&w, &["5a6b"]);
    write_master(
        &a,
        &[
            (ROOT, "5i4h", "main", "100"),
            (ROOT, "5i6h", "alt", "200"),
            (&w, "5a5b", "main", "100,200"),
            (&w, "5a4b", "alt", "-50"),
            (&w, "5a6b", "record", "0"),
            (&w, "5a6a", "bench", "0"),
            (&record, "4h3h", "main", "0"),
        ],
    );
    let (visited, exits) = walk(&a, ROOT);
    assert_eq!((visited, exits.len()), (4, 2));
    let main = exits.iter().find(|e| e.entry.mv == "5a5b").unwrap();
    assert_eq!(
        (main.ply, main.entry.value, main.entry.key.as_str()),
        (2, Some(-150), w.as_str())
    );
    assert_eq!(main.entry.source, "read");
    assert_eq!(exits.iter().find(|e| e.entry.mv == "5a4b").unwrap().entry.value, Some(50));
    a.side = Own::W;
    let (_, exits) = walk(&a, ROOT);
    assert_eq!(exits.len(), 2);
    assert!(
        exits
            .iter()
            .any(|e| e.key == after(ROOT, &["5i6h"]) && e.entry.value == Some(200))
    );
    assert!(!exits.iter().any(|e| e.entry.mv == "5a4b"));
}

#[test]
fn breadth_first_shortest_route_and_all_incoming_entries() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path(), "w");
    let long1 = after(ROOT, &["5i6h"]);
    let long2 = after(&long1, &["5a4b"]);
    let long3 = after(&long2, &["6h5h"]);
    let third1 = after(ROOT, &["5i4h"]);
    let third2 = after(&third1, &["5a5b"]);
    let third3 = after(&third2, &["4h4i"]);
    let third4 = after(&third3, &["5b5c"]);
    let third5 = after(&third4, &["4i5h"]);
    let short = after(ROOT, &["5i5h"]);
    let end = after(&short, &["5a5b"]);
    write_master(
        &a,
        &[
            (ROOT, "5i6h", "main", ""),
            (ROOT, "5i5h", "alt", ""),
            (ROOT, "5i4h", "alt", ""),
            (&long1, "5a4b", "main", ""),
            (&long2, "6h5h", "main", "10"),
            (&long3, "4b5b", "main", "40"),
            (&short, "5a5b", "main", "20"),
            (&third1, "5a5b", "main", ""),
            (&third2, "4h4i", "main", ""),
            (&third3, "5b5c", "main", ""),
            (&third4, "4i5h", "main", ""),
            (&third5, "5c5b", "main", "30"),
        ],
    );
    let (_, exits) = walk(&a, ROOT);
    assert_eq!(exits.len(), 1);
    assert_eq!((exits[0].key.as_str(), exits[0].ply), (end.as_str(), 2));
    assert_eq!(exits[0].entry.value, Some(-20));
    assert_eq!(exits[0].entries.len(), 3);
    assert!(table(&exits, "w").contains("4b5b:-40"));
    a.max_ply = 1;
    assert_eq!(walk(&a, ROOT).1.len(), 0);
    a.max_ply = 2;
    assert_eq!(walk(&a, ROOT).1[0].ply, 2);
}

#[test]
fn classification_boundaries_and_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path(), "b");
    a.target_black = Some(100);
    let mut e = Entry {
        value: Some(99),
        kind: "single".into(),
        ..Entry::default()
    };
    assert_eq!(classify(&e, 21, &a), Class::Short);
    e.value = Some(100);
    assert_eq!(classify(&e, 20, &a), Class::WeakShallow);
    assert_eq!(classify(&e, 21, &a), Class::Other);
    e.value = None;
    assert_eq!(classify(&e, 20, &a), Class::NovalueShallow);
    assert_eq!(classify(&e, 21, &a), Class::Other);
    e.value = Some(101);
    for kind in ["single", "unknown", "", "mean:2", "tree:5", "tree:0005"] {
        e.kind = kind.into();
        assert_eq!(classify(&e, 20, &a), Class::WeakShallow, "{kind}");
    }
    for kind in [
        "tree:6",
        "tree:10",
        "tree:0006",
        "tree:9999999999999999999999999",
    ] {
        e.kind = kind.into();
        assert_eq!(classify(&e, 20, &a), Class::Other, "{kind}");
    }
    a.side = Own::W;
    a.target_white = Some(100);
    assert_eq!(classify(&e, 100, &a), Class::Short);
    e.value = Some(100);
    assert_eq!(classify(&e, 100, &a), Class::Short);
    e.value = Some(99);
    assert_eq!(classify(&e, 100, &a), Class::Other);
    a.target_white = None;
    e.value = Some(1000);
    assert_eq!(classify(&e, 100, &a), Class::Other);
}

fn line(id: &str, key: &str, own: &str, depth: usize, hold: bool) -> Line {
    Line {
        group: None,
        id: id.into(),
        root: format!("sfen {key} 1"),
        roots: Vec::new(),
        own: own.into(),
        depth,
        hold,
        priority: 100,
        game: Vec::new(),
        opts: Opts::default(),
        memo: None,
    }
}

#[test]
fn exclusion_last_row_depth_hold_both_roots_and_no_game() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path(), "b");
    write_master(&a, &[(ROOT, "5i4h", "main", "")]);
    let exit = after(ROOT, &["5i4h"]);
    let p1 = dir.path().join("q1.jsonl");
    let p2 = dir.path().join("q2.jsonl");
    let mut game = line("game", ROOT, "b", 6, false);
    game.game = vec![vec!["5i4h".into()]];
    let initial = vec![
        line("held", &exit, "b", 6, true),
        line("shallow", &exit, "b", 5, false),
        line("opposite", &exit, "w", 6, false),
        game,
    ];
    std::fs::write(&p1, format_queue(&initial).unwrap()).unwrap();
    a.exclude_queue = vec![p1.clone()];
    let (_, exits) = walk(&a, ROOT);
    assert!(exits[0].covered_by.is_empty());
    assert_eq!(exits[0].covered_hold, BTreeSet::from(["held".into()]));
    assert_eq!(queue(&exits, &a).unwrap().len(), 1);
    a.skip_covered_hold = true;
    assert!(queue(&exits, &a).unwrap().is_empty());
    let mut both = line("both", &after(ROOT, &["5i6h"]), "bw", 6, false);
    both.roots = vec![format!("sfen {exit} 10")];
    std::fs::write(&p2, format_queue(&[line("held", &exit, "b", 6, false), both]).unwrap())
        .unwrap();
    a.exclude_queue.push(p2.clone());
    let (_, exits) = walk(&a, ROOT);
    assert_eq!(exits[0].covered_by, BTreeSet::from(["both".into(), "held".into()]));
    assert!(exits[0].covered_hold.is_empty());
    assert!(queue(&exits, &a).unwrap().is_empty());
    a.side = Own::W;
    assert!(coverage(&a).unwrap()[&exit].active.contains("both"));
    a.side = Own::B;
    a.exclude_queue = vec![p2.clone(), p1.clone()];
    assert_eq!(walk(&a, ROOT).1[0].covered_by, BTreeSet::from(["both".into()]));
    std::fs::write(
        &p2,
        format_queue(&[
            line("held", &exit, "b", 6, false),
            line("held", &exit, "b", 6, true),
        ])
        .unwrap(),
    )
    .unwrap();
    a.exclude_queue = vec![p2];
    assert_eq!(walk(&a, ROOT).1[0].covered_hold, BTreeSet::from(["held".into()]));
    a.exclude_min_depth = 7;
    let (_, exits) = walk(&a, ROOT);
    assert!(exits[0].covered_by.is_empty());
    assert!(exits[0].covered_hold.is_empty());
}

#[test]
fn priority_class_order_then_ply_then_key_and_enqueue_validation() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path(), "b");
    a.enqueue_class = vec![Class::Other, Class::WeakShallow, Class::Short];
    a.opp_n = Some(3);
    a.stop = Some(120);
    a.rounds = Some(2);
    a.draw_black = Some(-50);
    a.draw_white = Some(10);
    let keys = [
        ROOT.to_owned(),
        after(ROOT, &["5i4h", "5a5b"]),
        after(ROOT, &["5i6h", "5a5b"]),
    ];
    let make = |key: &str, ply, class| Exit {
        key: key.into(),
        ply,
        entry: Entry::default(),
        entries: BTreeMap::new(),
        class,
        covered_by: BTreeSet::new(),
        covered_hold: BTreeSet::new(),
    };
    let mut exits = vec![
        make(&keys[0], 0, Class::Short),
        make(&keys[1], 10, Class::Other),
        make(&keys[2], 4, Class::Other),
        make(&keys[0], 4, Class::Other),
    ];
    let lines = queue(&exits, &a).unwrap();
    let mut sorted = [keys[0].clone(), keys[2].clone()];
    sorted.sort();
    assert_eq!(parse_root(&lines[0].root).unwrap().key, sorted[0]);
    assert_eq!(parse_root(&lines[1].root).unwrap().key, sorted[1]);
    assert_eq!(parse_root(&lines[2].root).unwrap().ply, 11);
    assert_eq!(parse_root(&lines[3].root).unwrap().ply, 1);
    assert_eq!(lines.iter().map(|l| l.priority).collect::<Vec<_>>(), [10, 11, 12, 13]);
    assert_eq!(lines[0].opts, a.opts());
    assert_eq!(lines[0].id, queue_id("exits", 5, &sorted[0], "b"));
    assert!(
        lines[0]
            .memo
            .as_ref()
            .unwrap()
            .contains("class=other entry_value_black= entry_value_kind= entry_move=")
    );
    let path = dir.path().join("out.jsonl");
    std::fs::write(&path, format_queue(&lines).unwrap()).unwrap();
    let mut read = Vec::new();
    read_checked_lines(&path, |l, _, _| {
        read.push(l);
        Ok(())
    })
    .unwrap();
    assert_eq!(read, lines);
    exits[0].class = Class::NovalueShallow;
    assert_eq!(queue(&exits, &a).unwrap().len(), 3);
    a.priority = MAX_PRIORITY;
    assert!(queue(&exits, &a).is_err());
}

#[test]
fn start_moves_empty_exit_record_only_and_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path(), "b");
    write_master(&a, &[(HIRATE_KEY, "7g7f", "record", "42")]);
    a.enqueue_out = Some(dir.path().join("out.jsonl"));
    run(a).unwrap();
    let text = std::fs::read_to_string(dir.path().join("exits.tsv")).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(text.contains("\tnovalue-shallow\t"));
    read_checked_lines(&dir.path().join("out.jsonl"), |line, root, _| {
        assert_eq!(root.ply, 1);
        assert_eq!(line.own, "b");
        Ok(())
    })
    .unwrap();
    let mut a = args(dir.path(), "b");
    a.start_moves = Some("7g7f".into());
    let (_, exits) = walk(&a, HIRATE_KEY);
    assert_eq!(exits[0].entry.mv, "7g7f");
    assert_eq!(exits[0].entry.value, Some(42));
    a.start_moves = Some("7g7f 3c3d".into());
    a.max_ply = 2;
    let (_, exits) = walk(&a, HIRATE_KEY);
    assert_eq!((exits[0].ply, exits[0].entry.mv.as_str()), (2, "3c3d"));
    a.max_ply = 1;
    let mut index = MasterIndex::open(&a.master, &Schema::load(None).unwrap()).unwrap();
    assert!(traverse(&mut index, &a, HIRATE_KEY).is_err());
    a.max_ply = 200;
    a.start_moves = Some("7g7e".into());
    assert!(traverse(&mut index, &a, HIRATE_KEY).is_err());
}

#[test]
fn mean_and_extreme_negative_are_converted_without_overflow() {
    let row = test_row(&[
        ("key", after(ROOT, &["5i4h"])),
        ("evals", i32::MIN.to_string()),
    ]);
    assert_eq!(Entry::from_row(&row).unwrap().value, Some(2147483648));
    let mut row = row;
    row.set("evals", "10,30,20".into());
    row.set("value_kind", "mean:3".into());
    assert_eq!(Entry::from_row(&row).unwrap().value, Some(-20));
}

#[test]
fn rejects_output_aliases_and_bad_queue_options() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path(), "b");
    write_master(&a, &[]);
    a.table = a.master.clone();
    assert!(a.check().is_err());
    a.table = dir.path().join("out.tsv");
    a.enqueue_out = Some(dir.path().join(".").join("out.tsv"));
    assert!(a.check().is_err());
    a.enqueue_out = None;
    a.exclude_queue = vec![a.table.clone()];
    assert!(a.check().is_err());
    a.exclude_queue.clear();
    a.opp_n = Some(9);
    assert!(a.check().is_err());
    a.opp_n = None;
    a.enqueue_class.push(Class::Short);
    assert!(a.check().is_err());
    assert!(
        Cli::try_parse_from([
            "test",
            "--master",
            "moves.tsv",
            "--table",
            "out.tsv",
            "--side",
            "both"
        ])
        .is_err()
    );
}

#[test]
fn registered_cli_and_repeated_outputs_are_identical() {
    #[derive(Parser)]
    struct MasterCli {
        #[command(flatten)]
        master: crate::book_tree::MasterArgs,
    }
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("moves.tsv");
    let table = dir.path().join("exits.tsv");
    let out = dir.path().join("queue.jsonl");
    std::fs::write(&master, master_text(&[(HIRATE_KEY, "7g7f", "main", "10,20")])).unwrap();
    let command = [
        "test",
        "exits",
        "--master",
        master.to_str().unwrap(),
        "--table",
        table.to_str().unwrap(),
        "--side",
        "b",
        "--enqueue-out",
        out.to_str().unwrap(),
        "--target-black",
        "100",
    ];
    let cli = MasterCli::try_parse_from(command).unwrap();
    crate::book_tree::run_master(cli.master).unwrap();
    let first_table = std::fs::read(&table).unwrap();
    let first_queue = std::fs::read(&out).unwrap();
    let cli = MasterCli::try_parse_from(command).unwrap();
    crate::book_tree::run_master(cli.master).unwrap();
    assert_eq!(first_table, std::fs::read(&table).unwrap());
    assert_eq!(first_queue, std::fs::read(&out).unwrap());
    read_checked_lines(&out, |line, root, _| {
        assert_eq!(root.ply, 2);
        assert_eq!(line.priority, 10);
        assert!(line.memo.unwrap().contains("class=short entry_value_black=15"));
        Ok(())
    })
    .unwrap();
}

#[test]
fn cycles_stop_and_alt_only_is_not_an_exit_on_own_turn() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path(), "b");
    let w1 = after(ROOT, &["5i4h"]);
    let b1 = after(&w1, &["5a4b"]);
    let w2 = after(&b1, &["4h5i"]);
    write_master(
        &a,
        &[
            (ROOT, "5i4h", "main", ""),
            (&w1, "5a4b", "main", ""),
            (&b1, "4h5i", "main", ""),
            (&w2, "4b5a", "main", ""),
        ],
    );
    let (count, exits) = walk(&a, ROOT);
    assert_eq!(count, 4);
    assert!(exits.is_empty());
    write_master(&a, &[(ROOT, "5i4h", "alt", "")]);
    let (count, exits) = walk(&a, ROOT);
    assert_eq!(count, 1);
    assert!(exits.is_empty());
}

#[test]
fn external_sort_spills_merges_and_preserves_equal_key_order() {
    fn sorted() -> Vec<(Vec<String>, String)> {
        let mut sorter = Sorter::new();
        for i in (0..20_000).rev() {
            sorter.push(vec![format!("{:05}", i % 3000)], i.to_string()).unwrap();
        }
        let mut reader = sorter.finish().unwrap();
        let mut result = Vec::new();
        while let Some(record) = read_record(&mut reader).unwrap() {
            result.push((record.key, record.value));
        }
        result
    }
    let mut expected: Vec<_> = (0..20_000)
        .rev()
        .map(|i| (vec![format!("{:05}", i % 3000)], i.to_string()))
        .collect();
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(sorted(), expected);
    assert_eq!(sorted(), expected);
}

#[test]
fn seen_merges_each_layer_with_linear_reads_and_first_discovery_order() {
    for size in [10_000, 40_000] {
        let mut seen = Seen::new("position-00000000").unwrap();
        for layer in 0..3 {
            let mut candidates = Sorter::new();
            for i in (0..size).rev() {
                let key = format!("position-{:08}", layer * size / 2 + i);
                for duplicate in 0..2 {
                    candidates
                        .push(
                            vec![key.clone(), format!("{:020}", (size - i) * 2 + duplicate)],
                            format!("{i}:{duplicate}"),
                        )
                        .unwrap();
                }
            }
            let before = seen.reads;
            let (mut next, added) = seen.advance(candidates).unwrap();
            let old_count = if layer == 0 {
                1
            } else {
                size + (layer - 1) * size / 2
            };
            assert_eq!(seen.reads - before, old_count + 2 * size);
            assert_eq!(seen.merges, layer + 1);
            assert_eq!(added, if layer == 0 { size - 1 } else { size / 2 });
            let mut count = 0;
            let mut previous = None;
            while let Some(record) = read_record(&mut next).unwrap() {
                assert!(previous.as_ref().is_none_or(|p| p < &record.key));
                assert!(record.value.ends_with(":0"));
                previous = Some(record.key);
                count += 1;
            }
            assert_eq!(count, added);
        }
    }
}

#[test]
fn incoming_dedup_uses_last_row_but_representative_uses_first_row() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path(), "b");
    write_master(&a, &[(ROOT, "5i4h", "main", "10"), (ROOT, "5i4h", "main", "20")]);
    let (_, exits) = walk(&a, ROOT);
    assert_eq!(exits[0].entry.value, Some(10));
    assert_eq!(exits[0].entries.len(), 1);
    assert_eq!(exits[0].entries.values().next().unwrap().value, Some(20));
}

#[test]
fn streamed_outputs_match_collected_order_and_validation_failure_preserves_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path(), "w");
    let one = after(HIRATE_KEY, &["7g7f"]);
    let two = after(HIRATE_KEY, &["2g2f"]);
    write_master(
        &a,
        &[
            (HIRATE_KEY, "7g7f", "main", ""),
            (HIRATE_KEY, "2g2f", "alt", ""),
            (HIRATE_KEY, "5g5f", "alt", ""),
            (&one, "3c3d", "main", "-90"),
            (&two, "8c8d", "main", "-100"),
        ],
    );
    a.target_white = Some(80);
    a.enqueue_class = vec![Class::NovalueShallow, Class::Short];
    let (_, exits) = walk(&a, HIRATE_KEY);
    let expected_table = table(&exits, "w");
    let expected_queue = format_queue(&queue(&exits, &a).unwrap()).unwrap();
    let table_path = a.table.clone();
    let queue_path = dir.path().join("out.jsonl");
    a.enqueue_out = Some(queue_path.clone());
    run(a).unwrap();
    assert_eq!(std::fs::read_to_string(&table_path).unwrap(), expected_table);
    assert_eq!(std::fs::read_to_string(&queue_path).unwrap(), expected_queue);
    let mut a = args(dir.path(), "w");
    a.target_white = Some(80);
    a.priority = MAX_PRIORITY;
    a.enqueue_out = Some(queue_path.clone());
    assert!(run(a).is_err());
    assert_eq!(std::fs::read_to_string(table_path).unwrap(), expected_table);
    assert_eq!(std::fs::read_to_string(queue_path).unwrap(), expected_queue);
}

#[test]
fn seen_checks_full_keys_even_when_bucket_hashes_collide() {
    use sha2::{Digest, Sha256};
    let mut buckets = BTreeMap::new();
    let (first, second) = (0..100_000)
        .find_map(|i| {
            let key = format!("position-{i}");
            let hash = Sha256::digest(key.as_bytes());
            buckets.insert([hash[0], hash[1]], key.clone()).map(|old| (old, key))
        })
        .unwrap();
    let mut seen = Seen::new(&first).unwrap();
    let mut candidates = Sorter::new();
    for (i, key) in [&first, &second, &first, &second].into_iter().enumerate() {
        candidates.push(vec![key.clone(), format!("{i:020}")], i.to_string()).unwrap();
    }
    let (mut next, added) = seen.advance(candidates).unwrap();
    assert_eq!(added, 1);
    let record = read_record(&mut next).unwrap().unwrap();
    assert_eq!(record.key[1], second);
    assert_eq!(record.value, "1");
    assert!(read_record(&mut next).unwrap().is_none());
}

#[test]
fn representative_follows_parent_discovery_order_across_sorted_layers() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path(), "w");
    let left = after(ROOT, &["5i6h"]);
    let right = after(ROOT, &["5i4h"]);
    let left2 = after(&left, &["5a5b"]);
    let right2 = after(&right, &["5a5b"]);
    let joined = after(&left2, &["6h5h"]);
    assert_eq!(joined, after(&right2, &["4h5h"]));
    for reverse in [false, true] {
        let mut roots = vec![(ROOT, "5i6h", "main", ""), (ROOT, "5i4h", "alt", "")];
        if reverse {
            roots.reverse();
        }
        roots.extend([
            (left.as_str(), "5a5b", "main", ""),
            (right.as_str(), "5a5b", "main", ""),
            (left2.as_str(), "6h5h", "main", "10"),
            (right2.as_str(), "4h5h", "main", "20"),
        ]);
        write_master(&a, &roots);
        let (_, exits) = walk(&a, ROOT);
        assert_eq!(exits.len(), 1);
        assert_eq!(exits[0].key, joined);
        assert_eq!(exits[0].ply, 3);
        assert_eq!(exits[0].entry.value, Some(if reverse { 20 } else { 10 }));
        assert_eq!(exits[0].entries.len(), 2);
    }
}
