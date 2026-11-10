use super::*;
use crate::book_db::{BookDb, BookMove, PositionEntry, strip_ply};
use crate::book_tree::reply_graph::fixture::{ROOT, after, master_text, positions};
use rshogi_book::BookEntryMove;
use std::path::Path;

type Table = BTreeMap<String, Vec<(&'static str, i32)>>;

fn probe(table: Table) -> Probe<'static> {
    Probe::new(move |sfen: &str| {
        Ok(table
            .get(strip_ply(sfen))
            .into_iter()
            .flatten()
            .map(|(m, v)| BookEntryMove {
                move_usi: (*m).into(),
                value: *v,
                depth: 10,
                move_count: 1,
            })
            .collect())
    })
}

fn options() -> WalkOptions {
    WalkOptions {
        max_ply: 80,
        floor: 1e-5,
        cont_max: 100,
        opp: OppOptions {
            min_games: 3,
            book_diff: 30,
            book_best: 0.9,
            book_first: false,
        },
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

#[test]
fn a_cycle_counts_each_position_once_per_path() {
    let a = after(ROOT, &["5i4h"]);
    let b = after(&a, &["5a4b"]);
    let c = after(&b, &["4h5i"]);
    let master = positions(&[(ROOT, "5i4h", "main", "100"), (&b, "4h5i", "main", "50")]);
    // A で 5a4b に 0.9、5a6b に 0.1。C から 4b5a で根に戻るので、根と B を何度も通る。
    let table = BTreeMap::from([
        (a.clone(), vec![("5a4b", 10), ("5a6b", -10)]),
        (c, vec![("4b5a", 0)]),
    ]);
    let mut src = Sources {
        freq: None,
        book: Some(probe(table)),
    };
    let visits = side_visits(&master, &mut src, "b", ROOT, options()).unwrap();
    assert!(close(visits[ROOT].weight, 1.0));
    assert_eq!(visits[ROOT].ply, 0);
    assert!(close(visits[&b].weight, 0.9));
    assert_eq!(
        (visits[&b].ply, visits[&b].moves.clone()),
        (2, vec!["5i4h".into(), "5a4b".into()])
    );
    // 繰り返しを数えると根の重みは 1 を超える。
    let mut moves = Moves::default();
    let plain =
        visit_weights(&master, &mut src, "b", ROOT, options(), None, Some(&mut moves)).unwrap();
    assert!(plain[ROOT].weight > 1.5);
    let cycles = moves.cycles("b");
    assert_eq!(
        cycles.bits.keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([&ROOT.to_owned(), &b])
    );
}

fn write_book(path: &Path, table: &Table) {
    let mut db = BookDb::default();
    for (k, moves) in table {
        let moves = moves
            .iter()
            .map(|(m, v)| BookMove {
                move_usi: Some((*m).into()),
                ponder_usi: None,
                value: *v,
                depth: 10,
                count: 1,
            })
            .collect();
        db.entries.insert(
            k.clone(),
            PositionEntry {
                sfen: format!("{k} 1"),
                moves,
            },
        );
    }
    db.write(path).unwrap();
}

/// 先手: 根の main 5i4h (読み +100) は、B の main 4h3h (+20) を補いで伝播して +20。外部定跡は 5i4h を -100 と見る。
/// 相手の手は外部定跡で A は 5a4b、D は 4b4c。E は元データに無く、外部定跡の続きは 3h2h (+30)。
/// 後手: 根で外部定跡の 1 位 5i5h の後の G の main 5a5b (後手から -30) を、外部定跡は +60 と見る。
fn fixture(dir: &Path) -> (PathBuf, PathBuf, String) {
    let a = after(ROOT, &["5i4h"]);
    let b = after(&a, &["5a4b"]);
    let d = after(&b, &["4h3h"]);
    let e = after(&d, &["4b4c"]);
    let g = after(ROOT, &["5i5h"]);
    let master = dir.join("moves.tsv");
    std::fs::write(
        &master,
        master_text(&[
            (ROOT, "5i4h", "main", "100"),
            (&b, "4h3h", "main", "20"),
            (&g, "5a5b", "main", "-30"),
        ]),
    )
    .unwrap();
    let book = dir.join("ext.db");
    write_book(
        &book,
        &BTreeMap::from([
            (ROOT.to_owned(), vec![("5i4h", -100), ("5i5h", 50)]),
            (a, vec![("5a4b", 0)]),
            (d, vec![("4b4c", 0)]),
            (e.clone(), vec![("3h2h", 30)]),
            (g, vec![("5a5b", 60)]),
        ]),
    );
    (master, book, e)
}

fn args(dir: &Path, master: PathBuf, book: PathBuf, n: usize) -> OppBookDiffArgs {
    OppBookDiffArgs {
        master,
        schema: None,
        opp_book: book,
        side: SideArg::Both,
        opp_freq: None,
        min_games: 3,
        max_ply: 80,
        weight_floor: 1e-5,
        opp_book_diff: 30,
        opp_book_best: 0.9,
        opp_book_first: false,
        cont_max: 100,
        min_weight: 0.002,
        min_gap: 80,
        draw_value_black: -70,
        draw_value_white: 70,
        threads: 1,
        opp_records: None,
        records_min_rate: 3800.0,
        records_max_ply: 130,
        exclude_queue: Vec::new(),
        enqueue_out: Some(dir.join(format!("queue{n}.jsonl"))),
        depth: 6,
        priority: 300,
        tag: "t".into(),
        opp_n: 3,
        stop: 300,
        rounds: 3,
        draw_black: -70,
        draw_white: 70,
        table: Some(dir.join(format!("diff{n}.tsv"))),
        markdown: Some(dir.join(format!("diff{n}.md"))),
        title: String::new(),
    }
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap()
}

fn table_rows(text: &str) -> Vec<BTreeMap<String, String>> {
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap().split('\t').collect();
    lines
        .map(|l| {
            header
                .iter()
                .map(|h| h.to_string())
                .zip(l.split('\t').map(str::to_owned))
                .collect()
        })
        .collect()
}

#[test]
fn run_compares_propagated_main_with_the_external_book_and_queues_the_cut() {
    let dir = tempfile::tempdir().unwrap();
    let (master, book, e) = fixture(dir.path());
    run_from(args(dir.path(), master.clone(), book.clone(), 1), ROOT).unwrap();
    run_from(args(dir.path(), master, book, 2), ROOT).unwrap();
    assert_eq!(read(dir.path(), "diff1.tsv"), read(dir.path(), "diff2.tsv"));
    assert_eq!(read(dir.path(), "queue1.jsonl"), read(dir.path(), "queue2.jsonl"));
    let rows = table_rows(&read(dir.path(), "diff1.tsv"));
    assert_eq!(rows.len(), 2);
    let black = &rows[0];
    let get = |row: &BTreeMap<String, String>, k: &str| row[k].clone();
    assert_eq!(get(black, "side"), "b");
    assert_eq!((get(black, "ply"), get(black, "main")), ("1".into(), "5i4h".into()));
    assert_eq!((get(black, "ours"), get(black, "ours_black")), ("20".into(), "20".into()));
    assert_eq!(get(black, "main_from"), "propagated");
    assert_eq!((get(black, "ext_same"), get(black, "gap")), ("-100".into(), "120".into()));
    assert_eq!((get(black, "ext_best"), get(black, "reverse")), ("5i5h".into(), "30".into()));
    assert_eq!((get(black, "cut_end"), get(black, "cut_ply")), ("own".into(), "5".into()));
    assert_eq!(get(black, "cut_moves"), "5i4h 5a4b 4h3h 4b4c");
    assert_eq!((get(black, "cont"), get(black, "cont_end_black")), ("3h2h".into(), "30".into()));
    let white = &rows[1];
    assert_eq!((get(white, "side"), get(white, "ply")), ("w".into(), "2".into()));
    assert_eq!((get(white, "ours"), get(white, "ours_black")), ("-30".into(), "30".into()));
    assert_eq!(
        (get(white, "ext_same"), get(white, "ext_same_black")),
        ("60".into(), "-60".into())
    );
    assert_eq!((get(white, "gap"), get(white, "reverse")), ("-90".into(), "90".into()));
    assert_eq!(get(white, "cut_end"), "out");

    let lines: Vec<Line> = read(dir.path(), "queue1.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    // 逆向きだけの後手の局面は積まない。
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].root, format!("sfen {e} 5"));
    assert_eq!((lines[0].own.as_str(), lines[0].depth, lines[0].priority), ("b", 6, 300));
    assert_eq!(lines[0].game, vec![vec!["3h2h".to_owned()]]);
    assert_eq!(lines[0].opts, args(dir.path(), PathBuf::new(), PathBuf::new(), 0).queue_opts());
    assert_eq!(lines[0].memo.as_deref(), Some("opp-book-diff ply=1 gap=120 weight=1.00000"));

    let md = read(dir.path(), "diff1.md");
    assert!(md.contains("## RAMU が先手"), "{md}");
    assert!(md.contains("### 外部定跡の方が RAMU に悪い (差 80 点以上、1 局面)"), "{md}");
    assert!(
        md.contains("### 外部定跡の 1 位の方が RAMU に良い (差 80 点以上、1 局面)"),
        "{md}"
    );
    assert!(md.contains("| 1 手目 | 100.00 % | +120 点 |"), "{md}");
}

#[test]
fn queued_cuts_are_reported_and_not_queued_again() {
    let dir = tempfile::tempdir().unwrap();
    let (master, book, _) = fixture(dir.path());
    run_from(args(dir.path(), master.clone(), book.clone(), 1), ROOT).unwrap();
    let mut again = args(dir.path(), master, book, 2);
    again.exclude_queue = vec![dir.path().join("queue1.jsonl")];
    run_from(again, ROOT).unwrap();
    let first: Line =
        serde_json::from_str(read(dir.path(), "queue1.jsonl").lines().next().unwrap()).unwrap();
    assert_eq!(read(dir.path(), "queue2.jsonl"), "");
    let rows = table_rows(&read(dir.path(), "diff2.tsv"));
    assert_eq!(rows[0]["queued"], first.id);
}

#[test]
fn outputs_must_not_overwrite_inputs_or_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let (master, book, _) = fixture(dir.path());
    let mut same = args(dir.path(), master.clone(), book.clone(), 1);
    same.table = Some(book.clone());
    assert!(run_from(same, ROOT).is_err());
    let mut twice = args(dir.path(), master, book, 1);
    twice.markdown = twice.table.clone();
    assert!(run_from(twice, ROOT).is_err());
}
