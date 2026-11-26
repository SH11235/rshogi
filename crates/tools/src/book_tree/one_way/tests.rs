use super::super::{
    journal::{Pv, Reading, Record, Settings},
    master_table::{format_master, test_row},
};
use super::*;
use rshogi_core::{
    movegen::{MoveList, generate_legal_all},
    position::SFEN_HIRATE,
};

const LINE: &str = "7g7f 3c3d 2g2f 8c8d 2f2e 8d8e";
fn reading(key: &str, mv: &str, cp: i32, second: Option<i32>) -> Reading {
    let pos = position_from_sfen(&format!("{key} 1")).unwrap();
    let mut legal = MoveList::new();
    generate_legal_all(&pos, &mut legal);
    let alt = legal.iter().map(|m| m.to_usi()).find(|m| m != mv).unwrap();
    let mut multipv = vec![Pv {
        mv: mv.into(),
        cp,
        depth: 10,
    }];
    if let Some(cp) = second {
        multipv.push(Pv {
            mv: alt,
            cp,
            depth: 10,
        });
    }
    Reading {
        forced_move: None,
        tag: "fixture".into(),
        ply: 1,
        key: key.into(),
        nodes_limit: 100,
        nodes: Some(100),
        sec: 1.0,
        threads: 1,
        hash: 16,
        draw: BTreeMap::new(),
        multipv,
        bestmove: mv.into(),
    }
}
fn record(reading: Reading) -> Record {
    let version = "book-tree-v1/newgame-ready/node-calib-last-multipv-v1";
    let mut settings = Settings {
        fingerprint: String::new(),
        version: version.into(),
        engine_sha256: "fixture".into(),
        options: BTreeMap::new(),
        content_sha256: BTreeMap::new(),
        nodes: reading.nodes_limit,
        multipv: reading.multipv.len(),
        hash: 16,
        engine_version: None,
    };
    settings.fingerprint = settings.computed_fingerprint().unwrap();
    Record {
        kind: version.into(),
        settings,
        provenance: "fixture".into(),
        prefetch: false,
        reading,
    }
}
fn write_records(path: &std::path::Path, records: &[Record]) {
    let mut file = File::create(path).unwrap();
    for r in records {
        writeln!(file, "{}", serde_json::to_string(r).unwrap()).unwrap();
    }
}
fn fixture(dir: &std::path::Path) -> (OneWayArgs, Vec<Record>) {
    let mut rows = Vec::new();
    let mut records = Vec::new();
    let mut pos = position_from_sfen(SFEN_HIRATE).unwrap();
    for (i, mv) in LINE.split_whitespace().enumerate() {
        let key = strip_ply(&pos.to_sfen()).to_owned();
        rows.push(test_row(&[
            ("key", key.clone()),
            ("ply", (i + 1).to_string()),
            ("move", mv.into()),
            ("role", "main".into()),
        ]));
        records.push(record(reading(&key, mv, 200, Some(100))));
        apply_usi_move(&mut pos, mv).unwrap();
    }
    let args = OneWayArgs {
        master: dir.join("moves.tsv"),
        schema: None,
        journal: vec![dir.join("journal.jsonl")],
        side: Side::Both,
        gap: 100,
        min_run: 3,
        moves: vec![],
        max_rows: 100000,
        max_visits: 10_000_000,
        table: dir.join("out.tsv"),
    };
    std::fs::write(&args.master, format_master(&rows)).unwrap();
    write_records(&args.journal[0], &records);
    (args, records)
}
fn render(args: &OneWayArgs) -> (String, bool) {
    let graph = load_graph(args).unwrap();
    let reads =
        load_reads(&args.journal, &graph.keys().cloned().collect(), &mut Warnings::new()).unwrap();
    let mut out = Vec::new();
    let cut = intervals(&mut out, &graph, &reads, args).unwrap();
    (String::from_utf8(out).unwrap(), cut.is_some())
}
#[test]
fn one_way_gap_boundaries_and_mate() {
    let dir = tempfile::tempdir().unwrap();
    let (mut args, mut recs) = fixture(dir.path());
    args.moves = vec![LINE.into()];
    for (i, values) in [
        (200, Some(100)),
        (200, Some(101)),
        (200, None),
        (29997, Some(200)),
        (-200, Some(-29997)),
    ]
    .into_iter()
    .enumerate()
    {
        recs[i] =
            record(reading(&recs[i].reading.key, &recs[i].reading.bestmove, values.0, values.1));
    }
    recs.pop();
    write_records(&args.journal[0], &recs);
    let output = args.table.clone();
    run(args).unwrap();
    let text = std::fs::read_to_string(output).unwrap();
    let rows: Vec<Vec<_>> = text.lines().skip(1).map(|l| l.split('\t').collect()).collect();
    assert_eq!(rows.len(), 6);
    assert_eq!(
        rows.iter().map(|r| r[9]).collect::<Vec<_>>(),
        ["yes", "no", "unknown", "yes", "yes", "unknown"]
    );
    assert_eq!(rows[0][8], "100");
    assert_eq!(rows[1][8], "99");
    assert_eq!(rows[3][5], "-29997");
    assert_eq!(rows[4][7], "-29997");
    assert_eq!(rows[5][11], "unknown");
    assert_eq!(rows[2][11], "unknown");
    assert!([0, 1, 3, 4].iter().all(|&i| rows[i][11] == "yes"));
}
#[test]
fn one_way_runs_roles_minimum_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    let (mut args, _) = fixture(dir.path());
    let (text, cut) = render(&args);
    assert!(!cut);
    let rows: Vec<Vec<_>> = text.lines().skip(1).map(|l| l.split('\t').collect()).collect();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row[3], "3");
        assert_eq!(row[9].split_whitespace().count(), 4);
    }
    let b = rows.iter().find(|r| r[0] == "b").unwrap();
    assert_eq!(&b[1..3], ["1", "5"]);
    args.max_rows = 1;
    assert!(render(&args).1);
    args.max_rows = 100;
    args.min_run = 4;
    assert_eq!(render(&args).0.lines().count(), 1);
    args.min_run = 3;
    args.side = Side::B;
    let original = std::fs::read_to_string(&args.master).unwrap();
    for at in [0, 1] {
        for role in ["bench", "record"] {
            let mut rows = master_table::parse_master(&original).unwrap();
            rows[at].set("role", role.into());
            std::fs::write(&args.master, format_master(&rows)).unwrap();
            assert_eq!(render(&args).0.lines().count(), 1);
        }
    }
}
#[test]
fn one_way_selection_determinism_and_unrelated_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut args, recs) = fixture(dir.path());
    let mut deeper = recs[0].reading.clone();
    deeper.nodes_limit = 200;
    deeper.multipv[0].cp = 400;
    let deeper = record(deeper);
    let other = dir.path().join("a.jsonl");
    write_records(&other, &[deeper.clone(), recs[0].clone()]);
    args.journal.extend([other.clone(), other]);
    let graph = load_graph(&args).unwrap();
    let reads = load_reads(
        &args.journal,
        &BTreeSet::from([recs[0].reading.key.clone()]),
        &mut Warnings::new(),
    )
    .unwrap();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads.values().next().unwrap().0, deeper.reading);
    let mut one = deeper.reading.clone();
    one.nodes_limit += 1;
    one.multipv.truncate(1);
    write_records(&args.journal[1], &[record(one)]);
    let reads =
        load_reads(&args.journal, &graph.keys().cloned().collect(), &mut Warnings::new()).unwrap();
    assert_eq!(gap(&reads[&recs[0].reading.key].0), Some(100));
    let table = args.table.clone();
    let paths = args.journal.clone();
    run(args).unwrap();
    let first = std::fs::read(&table).unwrap();
    let (mut args, _) = fixture(dir.path());
    args.journal = paths.into_iter().rev().collect();
    run(args).unwrap();
    assert_eq!(first, std::fs::read(table).unwrap());
}
#[test]
fn one_way_branch_and_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let (mut args, recs) = fixture(dir.path());
    args.side = Side::B;
    args.min_run = 2;
    let mut graph = load_graph(&args).unwrap();
    let reads = recs
        .iter()
        .map(|r| (r.reading.key.clone(), (r.reading.clone(), r.settings.fingerprint.clone())))
        .collect();
    let k: Vec<_> = recs.iter().map(|r| r.reading.key.clone()).collect();
    graph.get_mut(&k[1]).unwrap().moves.insert("branch".into(), k[4].clone());
    let mut out = Vec::new();
    intervals(&mut out, &graph, &reads, &args).unwrap();
    assert_eq!(String::from_utf8(out).unwrap().lines().count(), 3);
    graph.get_mut(&k[5]).unwrap().moves.clear();
    graph.get_mut(&k[5]).unwrap().moves.insert("cycle".into(), k[0].clone());
    let mut out = Vec::new();
    intervals(&mut out, &graph, &reads, &args).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.lines().count() >= 2);
    assert!(
        text.lines()
            .skip(1)
            .all(|l| l.split('\t').nth(3).unwrap().parse::<usize>().unwrap() <= 3)
    );
}
#[test]
fn one_way_moves_alternative_and_input_protection() {
    let dir = tempfile::tempdir().unwrap();
    let (mut args, recs) = fixture(dir.path());
    args.moves = vec![recs[0].reading.multipv[1].mv.clone(), "7g7f".into()];
    let table = args.table.clone();
    run(args).unwrap();
    let text = std::fs::read_to_string(table).unwrap();
    assert!(text.lines().nth(1).unwrap().ends_with("\tno"));
    assert!(text.lines().nth(2).unwrap().starts_with("2\t1\tb\t"));
    let (mut args, _) = fixture(dir.path());
    args.table = args.journal[0].clone();
    assert!(run(args).is_err());
}
#[test]
fn one_way_cli() {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        master: crate::book_tree::MasterArgs,
    }
    let cli = Cli::try_parse_from([
        "master",
        "one-way",
        "--master",
        "m.tsv",
        "--journal",
        "a",
        "--journal",
        "b",
        "--table",
        "out",
    ])
    .unwrap();
    let crate::book_tree::MasterCommand::OneWay(args) = cli.master.command else {
        panic!("one-way")
    };
    assert_eq!(args.journal.len(), 2);
    assert_eq!(args.gap, 100);
    assert_eq!(args.min_run, 3);
    assert_eq!(args.max_rows, 100000);
    assert_eq!(args.max_visits, 10_000_000);
}

#[test]
fn one_way_reading_rank_ties_and_partial_tail() {
    let dir = tempfile::tempdir().unwrap();
    let (args, recs) = fixture(dir.path());
    let key = &recs[0].reading.key;
    let keys = BTreeSet::from([key.clone()]);
    let mut single = recs[0].reading.clone();
    single.multipv.truncate(1);
    let mut other = recs[0].clone();
    other.settings.engine_sha256 = "another".into();
    other.settings.fingerprint = other.settings.computed_fingerprint().unwrap();
    other.reading.multipv[0].cp = 600;
    let expected = if other.settings.fingerprint < recs[0].settings.fingerprint {
        &other
    } else {
        &recs[0]
    };
    for records in [
        vec![record(single.clone()), recs[0].clone(), other.clone()],
        vec![other.clone(), recs[0].clone(), record(single.clone())],
    ] {
        write_records(&args.journal[0], &records);
        let reads = load_reads(&args.journal, &keys, &mut Warnings::new()).unwrap();
        assert_eq!(reads[key].0, expected.reading);
    }
    let mut conflict = expected.clone();
    conflict.reading.multipv[0].cp += 1;
    write_records(&args.journal[0], &[expected.clone(), conflict]);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&args.journal[0])
        .unwrap()
        .write_all(b"{partial")
        .unwrap();
    assert_eq!(
        load_reads(&args.journal, &keys, &mut Warnings::new()).unwrap()[key].0,
        expected.reading
    );
}

#[test]
fn one_way_stopping_branch_keeps_shorter_interval() {
    let dir = tempfile::tempdir().unwrap();
    let (mut args, recs) = fixture(dir.path());
    args.side = Side::B;
    args.min_run = 2;
    let mut graph = load_graph(&args).unwrap();
    let reads = recs
        .iter()
        .map(|r| (r.reading.key.clone(), (r.reading.clone(), r.settings.fingerprint.clone())))
        .collect();
    graph
        .get_mut(&recs[3].reading.key)
        .unwrap()
        .moves
        .insert("exit".into(), "outside".into());
    let mut out = Vec::new();
    intervals(&mut out, &graph, &reads, &args).unwrap();
    let text = String::from_utf8(out).unwrap();
    let runs: Vec<_> = text.lines().skip(1).map(|l| l.split('\t').nth(3).unwrap()).collect();
    assert_eq!(runs, ["2", "3"]);
}

#[test]
fn one_way_first_rows_and_warning_totals() {
    let dir = tempfile::tempdir().unwrap();
    let (args, recs) = fixture(dir.path());
    let mut triple = recs[0].clone();
    triple.reading.multipv.push(Pv {
        mv: "2g2f".into(),
        cp: 0,
        depth: 10,
    });
    triple = record(triple.reading);
    let mut single = triple.clone();
    single.reading.multipv.truncate(1);
    let mut deep = record(single.reading.clone());
    deep.reading.nodes_limit = 1000;
    deep = record(deep.reading);
    let mut invalid = triple.clone();
    invalid.reading.bestmove = "invalid".into();
    for (first, later) in [(single.clone(), triple.clone()), (triple.clone(), single)] {
        write_records(
            &args.journal[0],
            &[
                deep.clone(),
                first.clone(),
                later.clone(),
                later,
                invalid.clone(),
                invalid.clone(),
            ],
        );
        let mut warnings = Warnings::new();
        let reads =
            load_reads(&args.journal, &BTreeSet::from([first.reading.key.clone()]), &mut warnings)
                .unwrap();
        if first.reading.multipv.len() == 1 {
            assert!(reads.is_empty());
        } else {
            assert_eq!(reads[&first.reading.key].0, first.reading);
        }
        let mut out = Vec::new();
        report_warnings(&mut out, &warnings).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!(
                "one-way: fingerprint={} invalid=2 conflicting=2\n",
                triple.settings.fingerprint
            )
        );
    }
}

#[test]
fn one_way_thirty_diamonds_and_visit_limit() {
    let dir = tempfile::tempdir().unwrap();
    let (mut args, recs) = fixture(dir.path());
    args.side = Side::B;
    let mut graph = Graph::new();
    let mut reads = Reads::new();
    // 各段で二つの応手が次の同一局面へ合流し、手順数だけが 2^30 に増える。
    for i in 0..=30 {
        let key = format!("node{i:02} b -");
        let opp = format!("node{i:02} w -");
        let mut read = recs[0].reading.clone();
        read.key = key.clone();
        graph.insert(
            key.clone(),
            Node {
                ply: 2 * i + 1,
                moves: BTreeMap::from([(read.bestmove.clone(), opp.clone())]),
            },
        );
        reads.insert(key, (read, String::new()));
        let moves = if i == 30 {
            BTreeMap::new()
        } else {
            BTreeMap::from([
                ("a".into(), format!("node{:02} b -", i + 1)),
                ("b".into(), format!("node{:02} b -", i + 1)),
            ])
        };
        graph.insert(
            opp,
            Node {
                ply: 2 * i + 2,
                moves,
            },
        );
    }
    args.min_run = 32;
    args.max_visits = 100;
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    assert_eq!(intervals(&mut out, &graph, &reads, &args).unwrap(), None);
    assert_eq!(String::from_utf8(out).unwrap().lines().count(), 1);
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    args.min_run = 31;
    let mut out = Vec::new();
    assert_eq!(intervals(&mut out, &graph, &reads, &args).unwrap(), Some("max-visits"));
    assert!(String::from_utf8(out).unwrap().lines().count() > 1);
}
