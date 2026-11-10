use super::super::reply_graph::fixture::{ROOT, after, master_text, positions};
use super::super::walk::Probe;
use super::*;
use crate::book_db::{BookDb, BookMove, PositionEntry, strip_ply};
use rshogi_book::BookEntryMove;
use std::path::Path;

type Table = Vec<(String, Vec<(&'static str, u64)>)>;

fn write_book(path: &Path, table: &Table) {
    let mut db = BookDb::default();
    for (key, moves) in table {
        let moves = moves
            .iter()
            .map(|(m, n)| BookMove {
                move_usi: Some((*m).into()),
                ponder_usi: None,
                value: 0,
                depth: 10,
                count: *n,
            })
            .collect();
        db.entries.insert(
            key.clone(),
            PositionEntry {
                sfen: format!("{key} 1"),
                moves,
            },
        );
    }
    db.write(path).unwrap();
}

fn hirate(moves: &[&str]) -> String {
    after(HIRATE_KEY, moves)
}

#[test]
fn candidates_need_the_own_turn_and_legal_moves() {
    let text = "# 注\nname\tside\toverride\nmain\tb\t\nS2\tb\t2g2f 8c8d:7g7f; :2g2f\n";
    let candidates = parse_candidates(text).unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates[0].overrides.is_empty());
    assert_eq!(candidates[1].overrides.len(), 2);
    assert_eq!(candidates[1].by_key[HIRATE_KEY], "2g2f");
    assert_eq!(candidates[1].by_key[&hirate(&["2g2f", "8c8d"])], "7g7f");
    for (text, needle) in [
        ("name\tside\n", "見出し"),
        ("name\tside\toverride\nx\tb\t2g2f:8c8d\n", "手番"),
        ("name\tside\toverride\nx\tw\t:7g7f\n", "手番"),
        ("name\tside\toverride\nx\tb\t:7g6f\n", "指せません"),
        ("name\tside\toverride\nx\tb\t\nx\tw\t\n", "重なって"),
        ("name\tside\toverride\nx\tb\t:7g7f;:2g2f\n", "重なって"),
        ("name\tside\toverride\nx\tc\t\n", "side"),
        ("name\tside\toverride\nx\tb\t7g7f\n", "override"),
        ("name\tside\toverride\n", "候補がありません"),
    ] {
        let err = format!("{:#}", parse_candidates(text).err().expect(text));
        assert!(err.contains(needle), "{text}: {err}");
    }
}

#[test]
fn danger_table_reads_side_and_key_by_column_name() {
    let key = hirate(&["7g7f"]);
    let text = format!("# 注\nply\tside\tkey\n2\tw\t{key}\n");
    let danger = parse_danger(&text).unwrap();
    assert!(danger.contains(&("w", key.clone())));
    assert!(parse_danger(&format!("ply\tkey\n2\t{key}\n")).is_err());
    assert!(parse_danger("side\tkey\nw\tnot-a-position\n").is_err());
}

#[test]
fn weighted_quantile_takes_the_first_value_reaching_the_share() {
    let values = [(60, 0.6), (50, 0.4)];
    assert_eq!(weighted_quantile(&values, 0.5), Some(60));
    assert_eq!(weighted_quantile(&values, 0.4), Some(50));
    assert_eq!(weighted_quantile(&[], 0.5), None);
}

/// 先手が 5i4h、後手が 5a4a か 5a6a (半々)。5a4a の後は 4h5i 4a5a で初めの局面に戻り、5a6a の後は定跡が切れる。
fn cycle_rows() -> (Vec<Row>, Vec<State>) {
    let p1 = after(ROOT, &["5i4h"]);
    let p2 = after(&p1, &["5a4a"]);
    let p3 = after(&p2, &["4h5i"]);
    let master = positions(&[(ROOT, "5i4h", "main", "0"), (&p2, "4h5i", "main", "0")]);
    let table: BTreeMap<String, Vec<(&str, u64)>> = BTreeMap::from([
        (p1, vec![("5a4a", 6), ("5a6a", 6)]),
        (p3, vec![("4a5a", 10)]),
    ]);
    let probe = || {
        let table = table.clone();
        Probe::new(move |sfen: &str| {
            Ok(table
                .get(strip_ply(sfen))
                .into_iter()
                .flatten()
                .map(|(m, n)| BookEntryMove {
                    move_usi: (*m).into(),
                    value: 0,
                    depth: 10,
                    move_count: *n,
                })
                .collect())
        })
    };
    let options = WalkOptions {
        max_ply: 40,
        floor: 1e-6,
        cont_max: 100,
        opp: OppOptions {
            min_games: 3,
            book_diff: 30,
            book_best: 0.9,
            book_first: false,
        },
    };
    let overrides = HashMap::new();
    let from = WalkFrom {
        start: ROOT,
        start_moves: &[],
        overrides: &overrides,
    };
    let mut keys = Keys::default();
    let mut src = Sources {
        freq: Some(probe()),
        book: None,
    };
    let mut collect = Collect {
        keys: &mut keys,
        edges: HashSet::new(),
    };
    walk_side_with(&master, &mut src, "b", &from, &mut collect, options).unwrap();
    let edges = collect.edges;
    let cycles = cycle_nodes(&edges, keys.names.len());
    assert_eq!(cycles.len(), 4);
    let none = HashSet::new();
    let mut tags = Tags {
        keys: &mut keys,
        states: Vec::new(),
        ids: HashMap::new(),
        branches: HashMap::new(),
        auto: &none,
        listed: &none,
        cycles: &cycles,
    };
    let rows = walk_side_with(&master, &mut src, "b", &from, &mut tags, options).unwrap();
    (rows, tags.states)
}

#[test]
fn repeated_weight_counts_only_the_share_that_comes_back() {
    let (rows, states) = cycle_rows();
    let repeated: f64 =
        rows.iter().filter(|r| states[r.tag as usize].repeated).map(|r| r.weight).sum();
    let total: f64 = rows.iter().map(|r| r.weight).sum();
    assert!((total - 1.0).abs() < 1e-9, "{total}");
    assert!((repeated - 0.5).abs() < 1e-4, "{repeated}");
}

struct Fixture {
    _dir: tempfile::TempDir,
    args: OpeningReportArgs,
}

/// 平手から。元データ: 初手 main 2g2f (+50)・alt 7g7f (+30)、2g2f 8c8d の後 main 2f2e (+60)。
/// 頻度: 2g2f の後 8c8d 6 局・3c3d 4 局、7g7f の後 8c8d 10 局、2g2f 8c8d 2f2e の後 8d8e 10 局。
/// 強い対局 (各 10 局): 2g2f 8c8d 2f2e 8d8e は後手勝ち、2g2f 8c8d 2f2e 3c3d と 2g2f 3c3d 7g7f 8c8d は先手勝ち。
/// 表: 2g2f 3c3d の後の局面 (先手)。
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name);
    let rows = [
        (HIRATE_KEY.to_owned(), "2g2f", "main", "50"),
        (HIRATE_KEY.to_owned(), "7g7f", "alt", "30"),
        (hirate(&["2g2f", "8c8d"]), "2f2e", "main", "60"),
    ];
    let rows: Vec<_> = rows.iter().map(|(k, m, r, e)| (k.as_str(), *m, *r, *e)).collect();
    std::fs::write(path("moves.tsv"), master_text(&rows)).unwrap();
    write_book(
        &path("freq.db"),
        &vec![
            (hirate(&["2g2f"]), vec![("8c8d", 6), ("3c3d", 4)]),
            (hirate(&["7g7f"]), vec![("8c8d", 10)]),
            (hirate(&["2g2f", "8c8d", "2f2e"]), vec![("8d8e", 10)]),
        ],
    );
    let game = |res: &str, moves: [&str; 4]| {
        let mv: Vec<_> = moves.iter().map(|m| serde_json::json!([m])).collect();
        let line = serde_json::json!({"br": 3900, "wr": 3900, "res": res, "mv": mv});
        format!("{line}\n").repeat(10)
    };
    let games = [
        game("w", ["2g2f", "8c8d", "2f2e", "8d8e"]),
        game("b", ["2g2f", "8c8d", "2f2e", "3c3d"]),
        game("b", ["2g2f", "3c3d", "7g7f", "8c8d"]),
    ];
    std::fs::write(path("games.jsonl"), games.concat()).unwrap();
    std::fs::write(path("danger.tsv"), format!("side\tkey\nb\t{}\n", hirate(&["2g2f", "3c3d"])))
        .unwrap();
    std::fs::write(
        path("candidates.tsv"),
        "name\tside\toverride\nmain\tb\t\nfirst76\tb\t:7g7f\nmain_w\tw\t\n",
    )
    .unwrap();
    let args = OpeningReportArgs {
        master: path("moves.tsv"),
        schema: None,
        candidates: path("candidates.tsv"),
        opp_freq: Some(path("freq.db")),
        opp_book: None,
        opp_records: Some(path("games.jsonl")),
        danger: Some(path("danger.tsv")),
        min_games: 3,
        max_ply: 80,
        weight_floor: 1e-5,
        opp_book_diff: 30,
        opp_book_best: 0.9,
        opp_book_first: false,
        cont_max: 100,
        records_min_rate: 3800.0,
        records_max_ply: 130,
        danger_games: 10,
        danger_score: 0.2,
        black_score: 0.8,
        exclude_queue: Vec::new(),
        top: 20,
        list_min_weight: 0.001,
        reply_min_share: 0.01,
        table: Some(path("report.tsv")),
        markdown: Some(path("report.md")),
        title: "t".into(),
    };
    Fixture { _dir: dir, args }
}

fn table_rows(path: &Path) -> Vec<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap().split('\t').collect();
    assert_eq!(header, TABLE_COLUMNS.split('\t').collect::<Vec<_>>());
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
fn report_compares_candidates_by_exit_values_and_danger() {
    let f = fixture();
    run(f.args.clone()).unwrap();
    let rows = table_rows(f.args.table.as_ref().unwrap());
    assert_eq!(rows.len(), 3);
    let main = &rows[0];
    // own の出口 2 つ: 2g2f 3c3d (重み 0.4、+50) と 2g2f 8c8d 2f2e 8d8e (0.6、+60)。
    assert_eq!(main["name"], "main");
    assert_eq!(main["override_weight"], "");
    assert_eq!(main["mean_black"], "56.0");
    assert_eq!((main["median"].as_str(), main["p10"].as_str()), ("60", "50"));
    assert_eq!((main["ge50"].as_str(), main["ge100"].as_str()), ("1.000000", "0.000000"));
    assert_eq!((main["own"].as_str(), main["own_lt20"].as_str()), ("1.000000", "1.000000"));
    assert_eq!(main["unknown"], "1.000000");
    assert_eq!(
        (main["danger_strong"].as_str(), main["danger_listed"].as_str()),
        ("0.600000", "0.400000")
    );
    assert_eq!(
        (main["danger_both"].as_str(), main["danger_any"].as_str()),
        ("0.000000", "1.000000")
    );
    assert_eq!((main["deviate"].as_str(), main["deviate_median"].as_str()), ("0.400000", "50"));
    assert_eq!((main["exit_games"].as_str(), main["exit_score"].as_str()), ("20", "0.500"));
    assert_eq!(main["black_below"], "0.600000");
    let first76 = &rows[1];
    assert_eq!(first76["override"], ":7g7f");
    assert_eq!(first76["override_weight"], "1.000000");
    assert_eq!(
        (first76["mean_black"].as_str(), first76["danger_any"].as_str()),
        ("30.0", "0.000000")
    );
    // 後手の候補は初期局面で相手 (先手) の手が分からない。
    let white = &rows[2];
    assert_eq!((white["out"].as_str(), white["no_value"].as_str()), ("1.000000", "1.000000"));
    assert_eq!((white["ge50"].as_str(), white["le80"].as_str()), ("", "0.000000"));
    let md = std::fs::read_to_string(f.args.markdown.as_ref().unwrap()).unwrap();
    for needle in [
        "# t の戦型の比較",
        "| main | 初手 ▲２六歩 (main) | — | +56 |",
        "| first76 | 初手 ▲７六歩 | 100.0% | +30 |",
        "| △８四歩 | 60.0% | +60 |",
        "| △３四歩 | 40.0% | +50 |",
        "| main | 5 | 60.0% | 10 局・0.00 | 強い対局だけ | ▲２六歩 △８四歩 ▲２五歩 △８五歩 |",
        "| main | 3 | 40.0% | 10 局・1.00 | 表だけ | ▲２六歩 △３四歩 |",
        "| 4 | 60.0% | main | +60 | unknown | なし | ▲２六歩 △８四歩 ▲２五歩 △８五歩 |",
    ] {
        assert!(md.contains(needle), "{needle}\n{md}");
    }
}

#[test]
fn report_rejects_outputs_that_overwrite_inputs() {
    let f = fixture();
    let mut args = f.args.clone();
    args.table = Some(args.master.clone());
    assert!(run(args).is_err());
    let mut args = f.args.clone();
    args.markdown = args.table.clone();
    assert!(run(args).is_err());
    let mut args = f.args.clone();
    (args.table, args.markdown) = (None, None);
    assert!(run(args).is_err());
}
