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
            book_first: false,
        },
    }
}

fn walk(src: &mut Sources, side: &'static str, options: WalkOptions) -> Vec<Row> {
    walk_side(&positions(&master_rows()), src, side, ROOT, &[], options).unwrap()
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
fn position_with_enough_games_but_only_rare_moves_stops_as_rare() {
    let a = after(ROOT, &["5i4h"]);
    let freq = BTreeMap::from([(a.clone(), vec![("5a5b", 0, 2), ("5a4b", 0, 2)])]);
    let rows = walk(&mut sources(Some(freq), None), "b", options());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].stop, Stop::Rare);
    assert!(close(rows[0].weight, 1.0));
}

#[test]
fn large_book_diff_keeps_every_move_without_overflow() {
    let moves: Vec<BookEntryMove> = [("a", -100), ("b", -200)]
        .iter()
        .map(|(m, v)| BookEntryMove {
            move_usi: (*m).into(),
            value: *v,
            depth: 0,
            move_count: 1,
        })
        .collect();
    let opp = OppOptions {
        book_diff: i32::MAX,
        ..options().opp
    };
    assert_eq!(book_moves(&moves, opp).len(), 2);
}

#[test]
fn book_first_uses_book_moves_and_falls_back_to_frequency_outside_the_book() {
    let a = after(ROOT, &["5i4h"]);
    let first = WalkOptions {
        opp: OppOptions {
            book_first: true,
            ..options().opp
        },
        ..options()
    };
    // 定跡に A があるので、局数の多い頻度 (5a5b 0.6・5a4b 0.3) でなく定跡の手 (0.9・0.1) で指す。
    let rows = walk(&mut sources(Some(freq()), Some(book())), "b", first);
    assert!(close(find(&rows, Stop::Own, &after(&a, &["5a5b"])).weight, 0.9));
    assert!(close(find(&rows, Stop::Own, &after(&a, &["5a4b"])).weight, 0.1));
    assert!(close(rows.iter().map(|r| r.weight).sum(), 1.0));
    // 定跡に無い局面は頻度で指す。
    let rows = walk(&mut sources(Some(freq()), Some(Table::new())), "b", first);
    assert!(close(find(&rows, Stop::Own, &after(&a, &["5a5b"])).weight, 0.6));
    assert!(close(find(&rows, Stop::Rare, &a).weight, 0.1));
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

#[test]
fn continuation_negates_the_lowest_value_of_the_second_player() {
    let w = after(ROOT, &["5i4h"]);
    let table = BTreeMap::from([(w.clone(), vec![("5a5b", i32::MIN, 1)])]);
    let (_, end) = continuation(&w, &mut probe(table), 100).unwrap();
    assert_eq!(end, Some(-i64::from(i32::MIN)));
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
    let lines = |rows: &[Row]| queue_lines(rows, &args, &Excluded::new()).unwrap().0;
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
        opp_book_first: false,
        cont_max: 100,
        enqueue_out: Some(dir.join(format!("queue{n}.jsonl"))),
        max_root_ply: None,
        exclude_queue: Vec::new(),
        opp_n: None,
        stop: None,
        rounds: None,
        draw_black: None,
        draw_white: None,
        depth: 1,
        priority: 300,
        tag: "x19".into(),
        game_from_book: true,
        start_moves: None,
        opp_records: None,
        records_min_rate: 3800.0,
        records_max_ply: 130,
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

#[test]
fn ybb_frequency_book_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (master, _, big) = write_inputs(dir.path());
    let ybb = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rshogi-book/testdata/small.ybb");
    let err = run_from(args(dir.path(), master, Some(ybb), big, 1), ROOT).unwrap_err();
    assert!(err.to_string().contains("--opp-freq"), "{err}");
}

#[test]
fn start_moves_walk_from_the_position_with_the_moves_in_front() {
    let a = after(ROOT, &["5i4h"]);
    let master = positions(&master_rows());
    let from_root =
        walk_side(&master, &mut sources(Some(freq()), Some(book())), "b", ROOT, &[], options())
            .unwrap();
    let (start, start_moves) = start_position(ROOT, Some(" 5i4h ")).unwrap();
    assert_eq!((start.as_str(), start_moves.as_slice()), (a.as_str(), &["5i4h".to_owned()][..]));
    // 根の main を前に指しただけなので、根から辿ったのと同じ行になる。
    let from_a = walk_side(
        &master,
        &mut sources(Some(freq()), Some(book())),
        "b",
        &start,
        &start_moves,
        options(),
    )
    .unwrap();
    assert_eq!(from_a, from_root);
}

#[test]
fn start_moves_outside_master_stop_there_as_own() {
    let master = positions(&master_rows());
    let (start, start_moves) = start_position(ROOT, Some("5i6h")).unwrap();
    let rows =
        walk_side(&master, &mut sources(None, Some(book())), "w", &start, &start_moves, options())
            .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].stop, rows[0].ply, rows[0].moves.clone()), (Stop::Own, 1, start_moves));
    assert!(close(rows[0].weight, 1.0));
}

#[test]
fn start_moves_write_full_moves_and_root_ply_to_queue() {
    let dir = tempfile::tempdir().unwrap();
    let (master, freq, big) = write_inputs(dir.path());
    let mut start = args(dir.path(), master, Some(freq), big, 1);
    start.side = SideArg::W;
    start.min_weight = 0.0;
    start.start_moves = Some("5i6h".into());
    run_from(start, ROOT).unwrap();
    let table = std::fs::read_to_string(dir.path().join("walk1.tsv")).unwrap();
    let row: Vec<&str> = table.lines().nth(1).unwrap().split('\t').collect();
    assert_eq!((row[0], row[1], row[2], row[9]), ("w", "own", "1", "5i6h"));
    let queue = std::fs::read_to_string(dir.path().join("queue1.jsonl")).unwrap();
    let lines: Vec<Line> = queue.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].root, format!("sfen {} 2", after(ROOT, &["5i6h"])));
    assert!(lines[0].memo.as_deref().unwrap().ends_with(" moves=5i6h"));
}

#[test]
fn bad_start_moves_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (master, freq, big) = write_inputs(dir.path());
    let err = start_position(ROOT, Some("5i4h 5i4h")).unwrap_err();
    assert!(format!("{err:#}").contains("--start-moves の 2 手目"), "{err:#}");
    let mut long = args(dir.path(), master, Some(freq), big, 1);
    long.max_ply = 1;
    long.start_moves = Some("5i4h 5a5b".into());
    let err = run_from(long, ROOT).unwrap_err().to_string();
    assert!(err.contains("--max-ply"), "{err}");
}

#[test]
fn empty_start_moves_start_at_the_initial_position_and_max_ply_bounds_them() {
    for empty in [None, Some(""), Some("  ")] {
        assert_eq!(start_position(ROOT, empty).unwrap(), (ROOT.to_owned(), Vec::new()));
    }
    let (start, start_moves) = start_position(ROOT, Some("5i4h")).unwrap();
    let mut at_limit = options();
    at_limit.max_ply = 1;
    let rows = walk_side(
        &positions(&master_rows()),
        &mut sources(Some(freq()), None),
        "b",
        &start,
        &start_moves,
        at_limit,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].stop, rows[0].ply), (Stop::Max, 1));
}

fn queue_args(dir: &Path) -> WalkArgs {
    args(dir, PathBuf::new(), None, PathBuf::new(), 1)
}

fn existing_line() -> Line {
    Line {
        group: None,
        id: "existing".into(),
        root: format!("sfen {ROOT} 9"),
        roots: Vec::new(),
        own: "b".into(),
        depth: 1,
        priority: 100,
        hold: false,
        game: Vec::new(),
        opts: Opts::default(),
        memo: None,
    }
}

fn exclusions(dir: &Path, lines: &[Line]) -> Excluded {
    let path = dir.join("exclude.jsonl");
    std::fs::write(&path, format_queue(lines).unwrap()).unwrap();
    excluded_positions(&[path]).unwrap()
}

#[test]
fn queue_max_root_ply_uses_the_merged_minimum_sfen_ply() {
    let dir = tempfile::tempdir().unwrap();
    let mut args = queue_args(dir.path());
    args.max_root_ply = Some(3);
    let rows = vec![
        own_row(ROOT, 8, 0.3),
        own_row(ROOT, 2, 0.3),
        own_row(&after(ROOT, &["5i4h", "5a5b"]), 4, 0.8),
    ];
    let (lines, _) = queue_lines(&rows, &args, &Excluded::new()).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].root, format!("sfen {ROOT} 3"));
    args.max_root_ply = Some(2);
    assert!(queue_lines(&rows, &args, &Excluded::new()).unwrap().0.is_empty());
}

#[test]
fn queue_excludes_roots_by_position_and_own_and_counts_every_matching_id() {
    let dir = tempfile::tempdir().unwrap();
    let args = queue_args(dir.path());
    let more = after(ROOT, &["5i4h", "5a5b"]);
    let mut line = existing_line();
    line.roots.push(format!("sfen {more} 33"));
    let mut other = line.clone();
    other.id = "other".into();
    let excluded = exclusions(dir.path(), &[line, other]);
    let mut white = own_row(ROOT, 0, 0.8);
    white.side = "w";
    let rows = vec![own_row(ROOT, 0, 0.8), own_row(&more, 2, 0.8), white];
    let (lines, counts) = queue_lines(&rows, &args, &excluded).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].own, "w");
    assert_eq!(counts.sides["b"], 2);
    assert_eq!(counts.ids, BTreeMap::from([("existing".into(), 2), ("other".into(), 2)]));
    assert!(counts.format().contains("w: 既存の木で除いたキューの行 0"));
}

#[test]
fn queue_excludes_game_positions_only_for_own_within_depth_plus_one() {
    let dir = tempfile::tempdir().unwrap();
    let args = queue_args(dir.path());
    let moves = ["5i4h", "5a5b", "4h3h", "5b5c"];
    let mut line = existing_line();
    line.depth = 2;
    line.game = vec![moves.iter().map(|m| (*m).into()).collect()];
    for own in ["b", "w", "bw"] {
        line.own = own.into();
        let excluded = exclusions(dir.path(), &[line.clone()]);
        for n in 1..=4 {
            for side in ["b", "w"] {
                let mut row = own_row(&after(ROOT, &moves[..n]), n, 1.0);
                row.side = side;
                let expected = n <= 3 && own.contains(side) && (n % 2 == 0) == (side == "b");
                let (lines, _) = queue_lines(&[row], &args, &excluded).unwrap();
                assert_eq!(lines.is_empty(), expected, "own={own} side={side} n={n}");
            }
        }
    }
}

#[test]
fn queue_exclusion_uses_last_id_across_files_and_ignores_depth_zero() {
    let dir = tempfile::tempdir().unwrap();
    let mut line = existing_line();
    let path1 = dir.path().join("first.jsonl");
    let path2 = dir.path().join("last.jsonl");
    std::fs::write(&path1, format_queue(&[line.clone()]).unwrap()).unwrap();
    line.depth = 0;
    assert!(exclusions(dir.path(), &[line.clone()]).is_empty());
    assert!(exclusions(dir.path(), &[existing_line(), line.clone()]).is_empty());
    assert!(!exclusions(dir.path(), &[line.clone(), existing_line()]).is_empty());
    std::fs::write(&path2, format_queue(&[line]).unwrap()).unwrap();
    assert!(excluded_positions(&[path1, path2]).unwrap().is_empty());
}

#[test]
fn queue_opts_are_optional_and_use_enqueue_ranges_even_without_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut args = queue_args(dir.path());
    let rows = [own_row(ROOT, 0, 1.0)];
    let serialized = |args: &WalkArgs| {
        let (lines, _) = queue_lines(&rows, args, &Excluded::new()).unwrap();
        serde_json::to_value(&lines[0]).unwrap()
    };
    assert!(serialized(&args).get("opts").is_none());
    args.opp_n = Some(8);
    args.stop = Some(30000);
    args.rounds = Some(0);
    args.draw_black = Some(-30000);
    args.draw_white = Some(30000);
    assert_eq!(
        serialized(&args)["opts"],
        serde_json::json!({
            "opp_n": 8, "stop": 30000, "rounds": 0, "draw_black": -30000, "draw_white": 30000
        })
    );
    args.opp_n = Some(0);
    assert!(check_args(&args).unwrap_err().to_string().contains("opp_n"));
}

#[test]
fn queue_exclusion_rejects_invalid_lines_including_superseded_and_unused_game_moves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid.jsonl");
    let mut invalid = existing_line();
    invalid.game = vec![vec!["5i4h".into(), "5a5b".into(), "1a1b".into()]];
    for text in [
        "{broken".to_owned(),
        format_queue(&[invalid.clone(), existing_line()]).unwrap(),
        {
            invalid.depth = 0;
            format_queue(&[invalid]).unwrap()
        },
        {
            let mut line = existing_line();
            line.own = "bad".into();
            format_queue(&[line]).unwrap()
        },
        {
            let mut line = existing_line();
            line.opts.stop = Some(0);
            format_queue(&[line]).unwrap()
        },
    ] {
        std::fs::write(&path, text).unwrap();
        let error = excluded_positions(std::slice::from_ref(&path)).unwrap_err();
        assert!(format!("{error:#}").contains("invalid.jsonl:1"));
    }
}

#[test]
fn walk_new_flags_require_enqueue_out() {
    use clap::Parser;
    #[derive(Parser, Debug)]
    struct Cli {
        #[command(flatten)]
        walk: WalkArgs,
    }
    for (flag, value) in [
        ("--max-root-ply", "29"),
        ("--exclude-queue", "queue.jsonl"),
        ("--opp-n", "3"),
        ("--stop", "500"),
        ("--rounds", "2"),
        ("--draw-black", "-100"),
        ("--draw-white", "100"),
    ] {
        let base = [
            "walk",
            "--master",
            "moves.tsv",
            "--table",
            "walk.tsv",
            flag,
            value,
        ];
        assert_eq!(
            Cli::try_parse_from(base).unwrap_err().kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert!(
            Cli::try_parse_from(base.into_iter().chain(["--enqueue-out", "out.jsonl"])).is_ok()
        );
    }
}

#[test]
fn run_filters_only_queue_and_protects_exclusion_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let (master, freq, big) = write_inputs(dir.path());
    let base = |n| args(dir.path(), master.clone(), Some(freq.clone()), big.clone(), n);
    run_from(base(1), ROOT).unwrap();
    let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
    let mut filtered = base(2);
    filtered.max_root_ply = Some(2);
    run_from(filtered, ROOT).unwrap();
    assert!(read("queue2.jsonl").is_empty());
    assert_eq!(read("walk1.tsv"), read("walk2.tsv"));
    let mut filtered = base(3);
    filtered.exclude_queue = vec![dir.path().join("queue1.jsonl")];
    run_from(filtered, ROOT).unwrap();
    assert!(read("queue3.jsonl").is_empty());
    assert_eq!(read("walk1.tsv"), read("walk3.tsv"));
    for table in [false, true] {
        let mut same = base(4);
        same.exclude_queue = vec![dir.path().join("queue1.jsonl")];
        if table {
            same.table = dir.path().join("queue1.jsonl");
        } else {
            same.enqueue_out = Some(dir.path().join("queue1.jsonl"));
        }
        assert!(run_from(same, ROOT).is_err());
    }
    let before = read("queue1.jsonl");
    let mut invalid = base(1);
    let path = dir.path().join("bad.jsonl");
    std::fs::write(&path, "{}\n").unwrap();
    invalid.exclude_queue = vec![path];
    assert!(run_from(invalid, ROOT).is_err());
    assert_eq!(read("queue1.jsonl"), before);
}

#[test]
fn table_entry_uses_first_eval_in_black_perspective_and_keeps_missing_empty() {
    let dir = tempfile::tempdir().unwrap();
    let a = after(ROOT, &["5i4h"]);
    let master = master_text(&[
        (ROOT, "5i4h", "main", "12,88"),
        (&a, "5a5b", "main", "34,90"),
        (&a, "5a6b", "alt", ""),
    ]);
    let path = dir.path().join("master.tsv");
    std::fs::write(&path, master).unwrap();
    let mut index = super::super::master_index::MasterIndex::open(
        &path,
        &super::super::schema::Schema::load(None).unwrap(),
    )
    .unwrap();
    let mut rows = Vec::new();
    for (stop, moves) in [
        (Stop::Own, vec!["5i4h"]),
        (Stop::Out, vec!["5i4h", "5a5b"]),
        (Stop::Rare, vec!["5i4h", "5a4b"]),
        (Stop::Tiny, vec![]),
        (Stop::Max, vec!["5i4h", "5a5b"]),
        (Stop::Max, vec!["5i4h", "5a6b"]),
    ] {
        rows.push(
            stop_row(
                &mut Sources::default(),
                "b",
                stop,
                moves.len(),
                1.0,
                ROOT,
                &moves.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            )
            .unwrap(),
        );
    }
    let table = format_table_with_entries(&rows, &mut index, ROOT, None).unwrap();
    let mut stats = vec![<[records::Reach; 3]>::default(); rows.len()];
    stats[1][0].games = 2;
    stats[1][0].score = 1.5;
    stats[1][0].black_wins = 1;
    stats[1][0].draws = 1;
    stats[1][0].after.insert(30, 2);
    let with = format_table_with_entries(&rows, &mut index, ROOT, Some(&stats)).unwrap();
    let widths: Vec<_> = with.lines().map(|l| l.split('\t').count()).collect();
    assert!(widths.iter().all(|&w| w == 25), "{widths:?}");
    let second: Vec<_> = with.lines().nth(2).unwrap().split('\t').collect();
    assert_eq!(&second[15..], &["2", "0.75", "0", "", "0", "", "30", "1", "0", "1"]);
    assert_eq!(
        with.lines()
            .skip(1)
            .map(|l| l.rsplitn(11, '\t').last().unwrap())
            .collect::<Vec<_>>(),
        table.lines().skip(1).collect::<Vec<_>>()
    );
    let lines: Vec<Vec<&str>> = table.lines().skip(1).map(|s| s.split('\t').collect()).collect();
    assert_eq!(&lines[0][11..13], &["5i4h", "12"]);
    assert_eq!(&lines[1][11..13], &["5a5b", "-34"]);
    assert_eq!(&lines[2][11..], &["5a4b", "", "", ""]);
    assert_eq!(&lines[3][11..], &["", "", "", ""]);
    assert_eq!(&lines[4][11..13], &["5a5b", "-34"]);
    assert_eq!(&lines[0][13..], &["unknown", "read"]);
    assert_eq!(&lines[5][11..], &["5a6b", "", "", "read"]);
}
