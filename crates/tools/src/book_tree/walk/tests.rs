use super::super::reply_graph::fixture::{ROOT, after, master_text, positions};
use super::*;
use crate::book_db::{BookMove, PositionEntry};

/// 根で自分 (先手) は 5i4h。相手の局面 A では頻度 5a5b 6 局・5a4b 3 局・5a6b 1 局。
fn freq() -> BookDb {
    let a = after(ROOT, &["5i4h"]);
    BookDb::from_reader(
        format!("#YANEURAOU-DB2016 1.00\nsfen {a} 2\n5a5b none 0 0 6\n5a4b none 0 0 3\n5a6b none 0 0 1\n")
            .as_bytes(),
    )
    .unwrap()
}

/// 大きい定跡。A で 5a5b +10・5a4b 0・5a6b -50。A の 5a5b の後 (先手番) で 4h3h +20、その後 (後手番) で 5b5c +15。
fn book() -> BTreeMap<String, Vec<(&'static str, i32)>> {
    let a = after(ROOT, &["5i4h"]);
    BTreeMap::from([
        (a.clone(), vec![("5a5b", 10), ("5a4b", 0), ("5a6b", -50)]),
        (after(&a, &["5a5b"]), vec![("4h3h", 20)]),
        (after(&a, &["5a5b", "4h3h"]), vec![("5b5c", 15)]),
    ])
}

fn lookup(sfen: &str) -> Result<Vec<BookEntryMove>> {
    Ok(book()
        .get(strip_ply(sfen))
        .into_iter()
        .flatten()
        .map(|(m, v)| BookEntryMove {
            move_usi: (*m).into(),
            value: *v,
            depth: 10,
        })
        .collect())
}

fn master_rows() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    vec![(ROOT, "5i4h", "main", "")]
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
        },
    }
}

fn find<'a>(rows: &'a [Row], stop: Stop, key: &str) -> &'a Row {
    rows.iter().find(|r| r.stop == stop && r.key == key).unwrap()
}

#[test]
fn frequency_splits_by_games_and_rare_moves_stop() {
    let a = after(ROOT, &["5i4h"]);
    let rows =
        walk_side(&positions(&master_rows()), Some(&freq()), None, "b", ROOT, options()).unwrap();
    let b = find(&rows, Stop::Own, &after(&a, &["5a5b"]));
    assert!((b.weight - 0.6).abs() < 1e-12);
    assert_eq!(b.moves, ["5i4h", "5a5b"]);
    assert_eq!(b.ply, 2);
    assert!((find(&rows, Stop::Own, &after(&a, &["5a4b"])).weight - 0.3).abs() < 1e-12);
    assert!((find(&rows, Stop::Rare, &a).weight - 0.1).abs() < 1e-12);
    let total: f64 = rows.iter().map(|r| r.weight).sum();
    assert!((total - 1.0).abs() < 1e-12);
}

#[test]
fn book_fills_positions_without_games_and_reports_continuation() {
    let a = after(ROOT, &["5i4h"]);
    let mut probe = BookProbe::new(lookup);
    let rows = walk_side(&positions(&master_rows()), None, Some(&mut probe), "b", ROOT, options())
        .unwrap();
    let b = find(&rows, Stop::Own, &after(&a, &["5a5b"]));
    assert!((b.weight - 0.9).abs() < 1e-12);
    assert_eq!(b.cont, ["4h3h", "5b5c"]);
    // 最後に引けた局面は後手番の +15 なので先手から見て -15。
    assert_eq!(b.cont_end_black, Some(-15));
    assert_eq!(b.book_moves, Some(1));
    assert!((find(&rows, Stop::Own, &after(&a, &["5a4b"])).weight - 0.1).abs() < 1e-12);
    assert!(rows.iter().all(|r| r.stop != Stop::Rare));
}

#[test]
fn rare_share_follows_book_when_games_suffice() {
    let a = after(ROOT, &["5i4h"]);
    let mut probe = BookProbe::new(lookup);
    let rows = walk_side(
        &positions(&master_rows()),
        Some(&freq()),
        Some(&mut probe),
        "b",
        ROOT,
        options(),
    )
    .unwrap();
    // 頻度で 5a5b 0.6・5a4b 0.3、5a6b (1 局) の 0.1 を大きい定跡で 5a5b 0.09・5a4b 0.01。
    assert!((find(&rows, Stop::Own, &after(&a, &["5a5b"])).weight - 0.69).abs() < 1e-12);
    assert!((find(&rows, Stop::Own, &after(&a, &["5a4b"])).weight - 0.31).abs() < 1e-12);
    assert!(rows.iter().all(|r| r.stop != Stop::Rare));
}

#[test]
fn unknown_opponent_move_stops_as_out() {
    let rows =
        walk_side(&positions(&master_rows()), Some(&freq()), None, "w", ROOT, options()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].stop, rows[0].ply), (Stop::Out, 0));
}

fn write_inputs(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let master = dir.join("moves.tsv");
    std::fs::write(&master, master_text(&master_rows())).unwrap();
    let mut db = BookDb::default();
    for (k, moves) in book() {
        db.entries.insert(
            k.clone(),
            PositionEntry {
                sfen: format!("{k} 1"),
                moves: moves
                    .iter()
                    .map(|(m, v)| BookMove {
                        move_usi: Some((*m).into()),
                        ponder_usi: None,
                        value: *v,
                        depth: 10,
                        count: 1,
                    })
                    .collect(),
            },
        );
    }
    let big = dir.join("big.db");
    db.write(&big).unwrap();
    (master, big)
}

fn args(dir: &std::path::Path, master: PathBuf, big: PathBuf, n: usize) -> WalkArgs {
    WalkArgs {
        master,
        table: dir.join(format!("walk{n}.tsv")),
        side: SideArg::Both,
        weight: WeightArgs {
            opp_freq: None,
            min_games: 3,
            max_ply: 80,
            weight_floor: 1e-5,
            min_weight: 0.5,
        },
        opp_book: Some(big),
        opp_book_diff: 30,
        opp_book_best: 0.9,
        cont_max: 100,
        enqueue_out: Some(dir.join(format!("queue{n}.jsonl"))),
        depth: 1,
        priority: 300,
        tag: "x19".into(),
        game_from_book: true,
    }
}

#[test]
fn run_writes_table_and_queue_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    let (master, big) = write_inputs(dir.path());
    run_from(args(dir.path(), master.clone(), big.clone(), 1), ROOT).unwrap();
    run_from(args(dir.path(), master, big, 2), ROOT).unwrap();
    let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
    assert_eq!(read("walk1.tsv"), read("walk2.tsv"));
    assert_eq!(read("queue1.jsonl"), read("queue2.jsonl"));
    let lines: Vec<Line> =
        read("queue1.jsonl").lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    // 重み 0.5 以上は 5a5b の後 (0.9) だけ。後手は根で相手の手が分からず止まる。
    assert_eq!(lines.len(), 1);
    let a = after(ROOT, &["5i4h"]);
    assert_eq!(lines[0].root, format!("sfen {} 3", after(&a, &["5a5b"])));
    assert_eq!((lines[0].own.as_str(), lines[0].priority, lines[0].depth), ("b", 300, 1));
    assert!(lines[0].id.starts_with("x19_") && lines[0].id.ends_with("_b"));
    assert_eq!(lines[0].game, vec![vec!["4h3h".to_owned(), "5b5c".to_owned()]]);
    assert!(read("walk1.tsv").starts_with("side\tstop\tply\tweight"));
}

#[test]
fn output_must_not_overwrite_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let (master, big) = write_inputs(dir.path());
    let mut same = args(dir.path(), master.clone(), big.clone(), 1);
    same.table = big.clone();
    assert!(run_from(same, ROOT).is_err());
    let mut same = args(dir.path(), master.clone(), big, 1);
    same.enqueue_out = Some(master);
    assert!(run_from(same, ROOT).is_err());
}
