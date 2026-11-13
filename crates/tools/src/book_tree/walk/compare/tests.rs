use super::super::super::master_table::COLUMNS;
use super::*;
use crate::book_db::{BookDb, BookMove, PositionEntry};
use tempfile::TempDir;

type Table = BTreeMap<String, Vec<(&'static str, i32, u64)>>;

fn probe(table: Table) -> Probe<'static> {
    Probe::new(move |sfen| {
        Ok(table
            .get(strip_ply(sfen))
            .into_iter()
            .flatten()
            .map(|(mv, value, count)| BookEntryMove {
                move_usi: (*mv).to_owned(),
                value: *value,
                depth: 10,
                move_count: *count,
            })
            .collect())
    })
}

fn write_book(path: &Path, table: Table) {
    let mut db = BookDb::default();
    for (key, moves) in table {
        db.entries.insert(
            key.clone(),
            PositionEntry {
                sfen: format!("{key} 1"),
                moves: moves
                    .into_iter()
                    .map(|(mv, value, count)| BookMove {
                        move_usi: Some(mv.to_owned()),
                        ponder_usi: None,
                        value,
                        depth: 10,
                        count,
                    })
                    .collect(),
            },
        );
    }
    db.write(path).unwrap();
}

fn master(path: &Path, rows: &[(&str, &str, &str, &str)]) {
    let mut text = COLUMNS.join("\t") + "\n";
    for (key, mv, evals, kind) in rows {
        writeln!(
            text,
            "{key}\t1\t{mv}\tmain\tread\t{evals}\t1\tunknown\t{kind}\ttest:fixture\twide\ttest\t"
        )
        .unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn args(dir: &Path) -> CompareArgs {
    CompareArgs {
        master: dir.join("master.tsv"),
        schema: None,
        opp_book: dir.join("external.db"),
        table: dir.join("report.tsv"),
        markdown: dir.join("report.md"),
        side: SideArg::Both,
        opp_book_diff: 0,
        opp_book_best: 0.9,
        max_ply: 160,
        weight_floor: 1e-5,
        cont_max: 100,
        start_moves: None,
        opp_records: None,
        records_min_rate: 3800.0,
        records_max_ply: 200,
    }
}

fn step(key: &str, moves: &[&str]) -> String {
    moves.iter().fold(key.to_owned(), |key, mv| advance(&key, mv).unwrap())
}

#[test]
fn ties_and_inclusive_difference_are_walked_without_frequency() {
    let d = TempDir::new().unwrap();
    let a = args(d.path());
    master(&a.master, &[(HIRATE_KEY, "7g7f", "10", "tree:6")]);
    let mut index = MasterIndex::open(&a.master, &Schema::default()).unwrap();
    let key = step(HIRATE_KEY, &["7g7f"]);
    let table: Table = BTreeMap::from([(
        key,
        vec![
            ("3c3d", 100, 0),
            ("8c8d", 100, 0),
            ("1c1d", 70, 0),
            ("9c9d", 69, 0),
        ],
    )]);
    let mut sources = Sources {
        freq: None,
        book: Some(probe(table)),
    };
    let from = WalkFrom {
        start: HIRATE_KEY,
        start_moves: &[],
        overrides: &HashMap::new(),
    };
    let walk = |index: &mut MasterIndex, src: &mut Sources, options| {
        walk_using(&mut |key| main_move(index, key), src, "b", &from, &mut NoTag, options).unwrap()
    };
    let rows = walk(&mut index, &mut sources, a.options());
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.stop == Stop::Own && (r.weight - 0.5).abs() < 1e-10));
    let mut options = a.options();
    options.opp.book_diff = 30;
    let rows = walk(&mut index, &mut sources, options);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|r| r.moves[1] == "1c1d"));
    assert!(!rows.iter().any(|r| r.moves[1] == "9c9d"));
    assert!((rows.iter().map(|r| r.weight).sum::<f64>() - 1.0).abs() < 1e-10);
    let mut options = a.options();
    options.max_ply = 2;
    assert!(walk(&mut index, &mut sources, options).iter().all(|r| r.stop == Stop::Max));
    let mut options = a.options();
    options.floor = 0.6;
    assert!(walk(&mut index, &mut sources, options).iter().all(|r| r.stop == Stop::Tiny));
    sources.book = Some(probe(Table::new()));
    let rows = walk(&mut index, &mut sources, a.options());
    assert_eq!((rows.len(), rows[0].stop), (1, Stop::Out));
}

fn row(side: &'static str, moves: &[&str]) -> Row {
    Row {
        side,
        stop: Stop::Own,
        ply: moves.len(),
        weight: 1.0,
        key: step(HIRATE_KEY, moves),
        tag: 0,
        moves: moves.iter().map(|m| (*m).to_owned()).collect(),
        games: None,
        book_moves: None,
        cont: Vec::new(),
        cont_end_black: None,
    }
}

#[test]
fn last_own_value_is_not_the_last_opponent_value_and_uses_black_sign() {
    let d = TempDir::new().unwrap();
    let a = args(d.path());
    let one = step(HIRATE_KEY, &["7g7f"]);
    let two = step(HIRATE_KEY, &["7g7f", "3c3d"]);
    master(
        &a.master,
        &[
            (HIRATE_KEY, "7g7f", "30,32", "mean:2"),
            (&one, "3c3d", "-10", "tree:6"),
            (&two, "2g2f", "200", "single"),
        ],
    );
    let mut index = MasterIndex::open(&a.master, &Schema::default()).unwrap();
    let black = last_own(&mut index, &row("b", &["7g7f", "3c3d"]), HIRATE_KEY).unwrap();
    assert_eq!((black.ply, black.value, black.kind.as_str()), (Some(1), Some(31), "mean:2"));
    let white = last_own(&mut index, &row("w", &["7g7f", "3c3d", "2g2f"]), HIRATE_KEY).unwrap();
    assert_eq!((white.ply, white.value, white.kind.as_str()), (Some(2), Some(10), "tree:6"));
    let unknown =
        last_own(&mut index, &row("b", &["7g7f", "3c3d", "1g1f", "8c8d"]), HIRATE_KEY).unwrap();
    assert_eq!((unknown.ply, unknown.value), (Some(3), None));
}

#[test]
fn continuation_distinguishes_out_limit_repetition_and_illegal() {
    let king = "9/4k4/9/9/9/9/9/4K4/9 b -";
    let moves = ["5h4h", "5b4b", "4h5h", "4b5b"];
    let mut table = Table::new();
    for (i, mv) in moves.iter().enumerate() {
        table.insert(step(king, &moves[..i]), vec![(*mv, 10, 0)]);
    }
    let mut p = probe(table);
    assert_eq!(follow(king, &mut p, 0).unwrap().stop, "max");
    assert_eq!(follow(king, &mut p, 3).unwrap().stop, "max");
    let r = follow(king, &mut p, 100).unwrap();
    assert_eq!((r.stop, r.moves.len(), r.values), ("repetition", 4, vec![10, -10, 10, -10]));
    let mut p = probe(BTreeMap::from([(HIRATE_KEY.to_owned(), vec![("7g7f", 50, 0)])]));
    let r = follow(HIRATE_KEY, &mut p, 1).unwrap();
    assert_eq!((r.stop, r.moves.len()), ("out", 1));
    let mut p = probe(BTreeMap::from([(HIRATE_KEY.to_owned(), vec![("5e5d", 999, 0)])]));
    let r = follow(HIRATE_KEY, &mut p, 10).unwrap();
    assert_eq!((r.stop, r.moves.len(), r.values.len()), ("illegal", 0, 0));
}

#[test]
fn report_is_deterministic_and_counts_rated_game_positions() {
    let d = TempDir::new().unwrap();
    let mut a = args(d.path());
    master(&a.master, &[(HIRATE_KEY, "7g7f", "20", "tree:6")]);
    let one = step(HIRATE_KEY, &["7g7f"]);
    let two = step(HIRATE_KEY, &["7g7f", "3c3d"]);
    write_book(
        &a.opp_book,
        BTreeMap::from([(one, vec![("3c3d", 11, 0)]), (two, vec![("2g2f", 10, 0)])]),
    );
    let records = d.path().join("records.jsonl");
    std::fs::write(
        &records,
        "{\"br\":4100,\"wr\":4200,\"res\":\"b\",\"mv\":[[\"7g7f\"],[\"3c3d\"]]}\n",
    )
    .unwrap();
    a.opp_records = Some(records);
    let read = |p: &Path| std::fs::read_to_string(p).unwrap();
    run(a).unwrap();
    let table = read(&d.path().join("report.tsv"));
    let report = read(&d.path().join("report.md"));
    assert!(report.contains("準備の線"));
    assert!(report.contains("▲７六歩"));
    assert!(report.contains("1 局・1.00"));
    let mut a = args(d.path());
    a.opp_records = Some(d.path().join("records.jsonl"));
    run(a).unwrap();
    assert_eq!(table, read(&d.path().join("report.tsv")));
    assert_eq!(report, read(&d.path().join("report.md")));
}

#[test]
fn rejects_output_over_input_and_non_finite_or_zero_branch_shares() {
    let d = TempDir::new().unwrap();
    let mut a = args(d.path());
    std::fs::write(&a.master, "unchanged").unwrap();
    a.table = a.master.clone();
    assert!(a.check().is_err());
    assert_eq!(std::fs::read_to_string(&a.master).unwrap(), "unchanged");
    let mut a = args(d.path());
    a.markdown = a.table.clone();
    assert!(a.check().is_err());
    let mut a = args(d.path());
    a.markdown = a.opp_book.clone();
    assert!(a.check().is_err());
    for best in [0.0, 1.0, f64::NAN] {
        let mut a = args(d.path());
        a.opp_book_best = best;
        assert!(a.check().is_err());
    }
    let mut a = args(d.path());
    a.weight_floor = f64::INFINITY;
    assert!(a.check().is_err());
}

#[test]
fn reached_master_groups_reject_duplicates_and_alt_without_main() {
    let d = TempDir::new().unwrap();
    let a = args(d.path());
    master(&a.master, &[(HIRATE_KEY, "7g7f", "20", "tree:6")]);
    let original = std::fs::read_to_string(&a.master).unwrap();
    for role in ["alt", "bench", "record"] {
        std::fs::write(&a.master, original.replace("\tmain\t", &format!("\t{role}\t"))).unwrap();
        let mut index = MasterIndex::open(&a.master, &Schema::default()).unwrap();
        if role == "alt" {
            assert!(main_move(&mut index, HIRATE_KEY).is_err());
            assert!(last_own(&mut index, &row("b", &["7g7f"]), HIRATE_KEY).is_err());
        } else {
            assert_eq!(main_move(&mut index, HIRATE_KEY).unwrap(), None);
        }
    }
    let duplicate = original.clone() + original.lines().nth(1).unwrap() + "\n";
    std::fs::write(&a.master, duplicate).unwrap();
    let mut index = MasterIndex::open(&a.master, &Schema::default()).unwrap();
    assert!(main_move(&mut index, HIRATE_KEY).is_err());
}

#[test]
fn illegal_external_best_is_not_marked_prepared_even_with_zero_cont_limit() {
    let d = TempDir::new().unwrap();
    let a = args(d.path());
    master(&a.master, &[]);
    let mut index = MasterIndex::open(&a.master, &Schema::default()).unwrap();
    let mut book = probe(BTreeMap::from([(HIRATE_KEY.to_owned(), vec![("5e5d", 99, 0)])]));
    assert!(!details(&row("b", &[]), &mut index, &mut book, HIRATE_KEY, 0).unwrap().prepared);
    let mut book = probe(BTreeMap::from([(HIRATE_KEY.to_owned(), vec![("7g7f", 99, 0)])]));
    assert!(details(&row("b", &[]), &mut index, &mut book, HIRATE_KEY, 0).unwrap().prepared);
}

#[test]
fn report_explains_forced_prefix_and_distinguishes_missing_offsets_and_continuation() {
    let d = TempDir::new().unwrap();
    let mut a = args(d.path());
    master(&a.master, &[]);
    a.start_moves = Some("7g7f 3c3d".into());
    a.opp_records = Some(d.path().join("games.jsonl"));
    a.records_min_rate = 4100.0;
    let mut r = row("b", &["7g7f", "3c3d"]);
    r.stop = Stop::Max;
    let mut index = MasterIndex::open(&a.master, &Schema::default()).unwrap();
    let mut book = probe(Table::new());
    let v = details(&r, &mut index, &mut book, HIRATE_KEY, 100).unwrap();
    let stats = vec![<[records::Reach; 3]>::default()];
    let table = format_table(&[r.clone()], &[v], Some(&stats));
    let header: Vec<_> = table.lines().next().unwrap().split('\t').collect();
    let fields: Vec<_> = table.lines().nth(1).unwrap().split('\t').collect();
    let get = |key| fields[header.iter().position(|name| *name == key).unwrap()];
    assert_eq!(get("cont_plies"), "");
    assert_eq!(get("n0"), "0");
    assert_eq!(get("n10"), "");
    assert_eq!(get("n20"), "");
    let v = details(&r, &mut index, &mut book, HIRATE_KEY, 100).unwrap();
    let report = markdown(&a, &[r], &[v], Some(&stats)).unwrap();
    assert!(report.contains("開始手順を強制: ▲７六歩 △３四歩"));
    assert!(report.contains("次の 3 手目から重み 1"));
    assert!(report.contains("条件付きの比較"));
    assert!(report.contains("最善だけの場合は 100%"));
    assert!(report.contains("両者 R4100 以上"));
    assert!(!report.contains("格下"));
    assert!(report.contains("0 局・不明 / 対象局面なし / 対象局面なし"));
}
