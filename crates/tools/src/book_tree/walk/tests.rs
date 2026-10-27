use super::super::reply_graph::fixture::{ROOT, after, master_text, positions};
use super::*;
use crate::book_db::{BookDb, BookMove, PositionEntry};

type Table = BTreeMap<String, Vec<(&'static str, i32, u64)>>;

/// 根で自分 (先手) は 5i4h。相手の局面 A では頻度 5a5b 6 局・5a4b 3 局・5a6b 1 局。
fn freq() -> Table {
    let a = after(ROOT, &["5i4h"]);
    BTreeMap::from([(a, vec![("5a5b", 0, 6), ("5a4b", 0, 3), ("5a6b", 0, 1)])])
}

/// 大きい定跡。A で 5a5b +10・5a4b 0・5a6b -50。A の 5a5b の後 (先手番) から最善が 4h3h +20、
/// 5b5c +15、3h2h +5 と続く。
fn book() -> Table {
    let a = after(ROOT, &["5i4h"]);
    BTreeMap::from([
        (a.clone(), vec![("5a5b", 10, 1), ("5a4b", 0, 1), ("5a6b", -50, 1)]),
        (after(&a, &["5a5b"]), vec![("4h3h", 20, 1)]),
        (after(&a, &["5a5b", "4h3h"]), vec![("5b5c", 15, 1)]),
        (after(&a, &["5a5b", "4h3h", "5b5c"]), vec![("3h2h", 5, 1)]),
    ])
}

fn probe(table: Table) -> Probe<'static> {
    Probe::new(move |sfen: &str| {
        Ok(table
            .get(strip_ply(sfen))
            .into_iter()
            .flatten()
            .map(|(m, v, n)| BookEntryMove {
                move_usi: (*m).into(),
                value: *v,
                depth: 10,
                move_count: *n,
            })
            .collect())
    })
}

fn sources(freq: Option<Table>, book: Option<Table>) -> Sources<'static> {
    Sources {
        freq: freq.map(probe),
        book: book.map(probe),
    }
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

fn walk(src: &mut Sources, side: &'static str, options: WalkOptions) -> Vec<Row> {
    walk_side(&positions(&master_rows()), src, side, ROOT, options).unwrap()
}

fn find<'a>(rows: &'a [Row], stop: Stop, key: &str) -> &'a Row {
    rows.iter().find(|r| r.stop == stop && r.key == key).unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

#[test]
fn frequency_splits_by_games_and_rare_moves_stop() {
    let a = after(ROOT, &["5i4h"]);
    let rows = walk(&mut sources(Some(freq()), None), "b", options());
    let b = find(&rows, Stop::Own, &after(&a, &["5a5b"]));
    assert!(close(b.weight, 0.6));
    assert_eq!(b.moves, ["5i4h", "5a5b"]);
    assert_eq!((b.ply, b.games), (2, Some(0)));
    assert!(close(find(&rows, Stop::Own, &after(&a, &["5a4b"])).weight, 0.3));
    assert!(close(find(&rows, Stop::Rare, &a).weight, 0.1));
    assert!(close(rows.iter().map(|r| r.weight).sum(), 1.0));
}

#[test]
fn illegal_frequency_move_goes_to_rare() {
    let a = after(ROOT, &["5i4h"]);
    let freq = BTreeMap::from([(a.clone(), vec![("5a5b", 0, 6), ("1a1b", 0, 4)])]);
    let rows = walk(&mut sources(Some(freq), None), "b", options());
    assert!(close(find(&rows, Stop::Own, &after(&a, &["5a5b"])).weight, 0.6));
    assert!(close(find(&rows, Stop::Rare, &a).weight, 0.4));
}

#[test]
fn book_fills_positions_without_games_and_reports_continuation() {
    let a = after(ROOT, &["5i4h"]);
    let rows = walk(&mut sources(None, Some(book())), "b", options());
    let b = find(&rows, Stop::Own, &after(&a, &["5a5b"]));
    assert!(close(b.weight, 0.9));
    assert_eq!(b.cont, ["4h3h", "5b5c", "3h2h"]);
    // 最後に引けた局面は先手番の +5。
    assert_eq!(b.cont_end_black, Some(5));
    assert_eq!(b.book_moves, Some(1));
    assert!(close(find(&rows, Stop::Own, &after(&a, &["5a4b"])).weight, 0.1));
    assert!(rows.iter().all(|r| r.stop != Stop::Rare));
}

#[test]
fn rare_share_follows_book_when_games_suffice() {
    let a = after(ROOT, &["5i4h"]);
    let rows = walk(&mut sources(Some(freq()), Some(book())), "b", options());
    // 頻度で 5a5b 0.6・5a4b 0.3、5a6b (1 局) の 0.1 を大きい定跡で 5a5b 0.09・5a4b 0.01。
    assert!(close(find(&rows, Stop::Own, &after(&a, &["5a5b"])).weight, 0.69));
    assert!(close(find(&rows, Stop::Own, &after(&a, &["5a4b"])).weight, 0.31));
    assert!(rows.iter().all(|r| r.stop != Stop::Rare));
}

#[test]
fn unknown_opponent_move_stops_as_out() {
    let rows = walk(&mut sources(Some(freq()), None), "w", options());
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].stop, rows[0].ply), (Stop::Out, 0));
}

#[test]
fn second_player_reaches_own_position() {
    let book = BTreeMap::from([(ROOT.to_owned(), vec![("5i4h", 0, 1), ("5i6h", -10, 1)])]);
    let rows = walk(&mut sources(None, Some(book)), "w", options());
    let own = find(&rows, Stop::Own, &after(ROOT, &["5i4h"]));
    assert_eq!((own.ply, own.side), (1, "w"));
    assert!(close(own.weight, 0.9));
    assert!(close(find(&rows, Stop::Own, &after(ROOT, &["5i6h"])).weight, 0.1));
}

#[test]
fn weight_floor_and_max_ply_stop() {
    let a = after(ROOT, &["5i4h"]);
    let floor = WalkOptions {
        floor: 0.5,
        ..options()
    };
    let rows = walk(&mut sources(Some(freq()), None), "b", floor);
    assert!(close(find(&rows, Stop::Own, &after(&a, &["5a5b"])).weight, 0.6));
    assert!(close(find(&rows, Stop::Tiny, &after(&a, &["5a4b"])).weight, 0.3));
    let max = WalkOptions {
        max_ply: 1,
        ..options()
    };
    let rows = walk(&mut sources(Some(freq()), None), "b", max);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].stop, rows[0].ply, rows[0].key.as_str()), (Stop::Max, 1, a.as_str()));
}

#[test]
fn zero_share_moves_are_not_walked() {
    let a = after(ROOT, &["5i4h"]);
    for (best, kept, dropped) in [(1.0, "5a5b", "5a4b"), (0.0, "5a4b", "5a5b")] {
        let options = WalkOptions {
            floor: 0.0,
            opp: OppOptions {
                book_best: best,
                ..options().opp
            },
            ..options()
        };
        let rows = walk(&mut sources(None, Some(book())), "b", options);
        assert!(close(find(&rows, Stop::Own, &after(&a, &[kept])).weight, 1.0));
        assert!(rows.iter().all(|r| r.key != after(&a, &[dropped])), "best={best}");
    }
}

#[test]
fn equal_best_moves_share_the_best_weight() {
    let moves: Vec<BookEntryMove> = [("a", 10), ("b", 10), ("c", -15), ("d", -21)]
        .iter()
        .map(|(m, v)| BookEntryMove {
            move_usi: (*m).into(),
            value: *v,
            depth: 0,
            move_count: 1,
        })
        .collect();
    let got = book_moves(&moves, options().opp);
    let want = [("a", 0.45), ("b", 0.45), ("c", 0.1)];
    assert_eq!(got.len(), want.len());
    for ((m, w), (wm, ww)) in got.iter().zip(want) {
        assert_eq!(m, wm);
        assert!(close(*w, ww));
    }
}

#[test]
fn continuation_stops_at_cycle_limit_and_illegal_move() {
    let b = after(ROOT, &["5i4h", "5a5b"]);
    let cycle = ["4h3h", "5b4b", "3h4h", "4b5b"];
    let mut table = Table::new();
    let mut key = b.clone();
    for mv in cycle {
        table.insert(key.clone(), vec![(mv, 1, 1)]);
        key = after(&key, &[mv]);
    }
    assert_eq!(key, b);
    let mut book = probe(table.clone());
    assert_eq!(continuation(&b, &mut book, 100).unwrap().0, cycle);
    assert_eq!(continuation(&b, &mut book, 2).unwrap().0, cycle[..2]);
    let illegal = BTreeMap::from([(b.clone(), vec![("1a1b", 7, 1)])]);
    assert_eq!(continuation(&b, &mut probe(illegal), 100).unwrap(), (Vec::new(), Some(7)));
}

fn own_row(key: &str, ply: usize, weight: f64) -> Row {
    Row {
        side: "b",
        stop: Stop::Own,
        ply,
        weight,
        key: key.to_owned(),
        moves: vec![format!("p{ply}")],
        games: None,
        book_moves: None,
        cont: Vec::new(),
        cont_end_black: None,
    }
}

#[test]
fn queue_merges_plies_into_the_shortest_root_with_a_stable_id() {
    let dir = tempfile::tempdir().unwrap();
    let args = args(dir.path(), PathBuf::new(), None, PathBuf::new(), 1);
    let key = after(ROOT, &["5i4h", "5a5b"]);
    let lines = |rows: &[Row]| queue_lines(rows, &args).unwrap();
    let first = lines(&[own_row(&key, 4, 0.4), own_row(&key, 2, 0.2)]);
    let swapped = lines(&[own_row(&key, 4, 0.2), own_row(&key, 2, 0.4)]);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].root, format!("sfen {key} 3"));
    assert!(first[0].id.starts_with("x19_3_"));
    assert_eq!(first[0].id, swapped[0].id);
    assert_eq!(first[0].memo.as_deref(), Some("walk weight=0.60000 moves=p2"));
    let summary = format_summary(&[own_row(&key, 4, 0.4), own_row(&key, 2, 0.2)], 0.5, None);
    assert!(summary.contains("b: 読み始める局面 1 (重み 0.5 以上 1)"), "{summary}");
}

fn write_book(path: &Path, table: Table) {
    let mut db = BookDb::default();
    for (k, moves) in table {
        let moves = moves
            .iter()
            .map(|(m, v, n)| BookMove {
                move_usi: Some((*m).into()),
                ponder_usi: None,
                value: *v,
                depth: 10,
                count: *n,
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

fn write_inputs(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let master = dir.join("moves.tsv");
    std::fs::write(&master, master_text(&master_rows())).unwrap();
    let (freq_path, big) = (dir.join("freq.db"), dir.join("big.db"));
    write_book(&freq_path, freq());
    write_book(&big, book());
    (master, freq_path, big)
}

fn args(dir: &Path, master: PathBuf, freq: Option<PathBuf>, big: PathBuf, n: usize) -> WalkArgs {
    WalkArgs {
        master,
        table: dir.join(format!("walk{n}.tsv")),
        side: SideArg::Both,
        opp_freq: freq,
        min_games: 3,
        max_ply: 80,
        weight_floor: 1e-5,
        min_weight: 0.5,
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
    let (master, freq, big) = write_inputs(dir.path());
    let run = |n| {
        run_from(args(dir.path(), master.clone(), Some(freq.clone()), big.clone(), n), ROOT)
            .unwrap()
    };
    run(1);
    run(2);
    let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
    assert_eq!(read("walk1.tsv"), read("walk2.tsv"));
    assert_eq!(read("queue1.jsonl"), read("queue2.jsonl"));
    let lines: Vec<Line> =
        read("queue1.jsonl").lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    // 重み 0.5 以上は 5a5b の後 (頻度 0.6 と定跡 0.09) だけ。後手は根で相手の手が分からず止まる。
    assert_eq!(lines.len(), 1);
    let a = after(ROOT, &["5i4h"]);
    assert_eq!(lines[0].root, format!("sfen {} 3", after(&a, &["5a5b"])));
    assert_eq!((lines[0].own.as_str(), lines[0].priority, lines[0].depth), ("b", 300, 1));
    assert!(lines[0].id.starts_with("x19_3_") && lines[0].id.ends_with("_b"));
    // game は段数 + 1 手で切る。
    assert_eq!(lines[0].game, vec![vec!["4h3h".to_owned(), "5b5c".to_owned()]]);
    assert!(lines[0].memo.as_deref().unwrap().starts_with("walk weight=0.69000 "));
    assert!(read("walk1.tsv").starts_with("side\tstop\tply\tweight"));
}

#[test]
fn outputs_must_not_overwrite_inputs_or_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let (master, freq, big) = write_inputs(dir.path());
    let base = || args(dir.path(), master.clone(), Some(freq.clone()), big.clone(), 1);
    let mut same = base();
    same.table = freq.clone();
    assert!(run_from(same, ROOT).is_err());
    let mut same = base();
    same.enqueue_out = Some(master.clone());
    assert!(run_from(same, ROOT).is_err());
    let mut same = base();
    same.enqueue_out = Some(dir.path().join("sub").join("..").join("walk1.tsv"));
    let err = run_from(same, ROOT).unwrap_err().to_string();
    assert!(err.contains("--table"), "{err}");
}

#[test]
fn bad_tag_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (master, freq, big) = write_inputs(dir.path());
    for tag in ["", "a.b", &"t".repeat(MAX_TAG_LEN + 1)] {
        let mut bad = args(dir.path(), master.clone(), Some(freq.clone()), big.clone(), 1);
        bad.tag = tag.to_owned();
        assert!(check_args(&bad).is_err(), "{tag}");
    }
    let mut ok = args(dir.path(), master, Some(freq), big, 1);
    ok.tag = "t".repeat(MAX_TAG_LEN);
    assert!(check_args(&ok).is_ok());
}
