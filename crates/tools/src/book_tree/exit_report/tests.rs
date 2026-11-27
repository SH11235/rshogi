use super::super::master_table::{format_master, test_row};
use super::*;
use clap::Parser;
use tempfile::TempDir;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: ExitReportArgs,
}

fn key_after(moves: &[&str]) -> String {
    moves.iter().fold(HIRATE_KEY.to_owned(), |key, mv| child(&key, mv).unwrap())
}

fn master_row(
    moves: &[&str],
    mv: &str,
    evals: &str,
    kind: &str,
) -> super::super::master_table::Row {
    test_row(&[
        ("key", key_after(moves)),
        ("move", mv.into()),
        ("role", "main".into()),
        ("evals", evals.into()),
        ("value_kind", kind.into()),
    ])
}

const WALK_HEADER: &str = "side\tstop\tply\tweight\tgames\tbook_moves\tcont_plies\tcont_end_black\tkey\tmoves\tcont\tentry_move\tentry_value_black\tentry_value_kind\tentry_source";

fn walk_line(side: &str, stop: &str, weight: f64, moves: &[&str]) -> String {
    format!(
        "{side}\t{stop}\t{}\t{weight}\t\t\t\t\t{}\t{}\t\t\t\t\t",
        moves.len(),
        key_after(moves),
        moves.join(" ")
    )
}

fn run_report(d: &TempDir, walk: &[String], extra: &[&str]) -> (String, String, String) {
    let rows = [
        master_row(&[], "7g7f", "60", "tree:3"),
        master_row(&["7g7f", "3c3d"], "2g2f", "40", "tree:2"),
        master_row(&["7g7f", "3c3d", "2g2f"], "8c8d", "-90", "single"),
        master_row(&["2g2f"], "8c8d", "10", "single"),
    ];
    run_report_with_rows(d, walk, extra, &rows)
}

fn run_report_with_rows(
    d: &TempDir,
    walk: &[String],
    extra: &[&str],
    rows: &[super::super::master_table::Row],
) -> (String, String, String) {
    let master = d.path().join("moves.tsv");
    std::fs::write(&master, format_master(rows)).unwrap();
    let walk_path = d.path().join("walk.tsv");
    std::fs::write(&walk_path, format!("{WALK_HEADER}\n{}\n", walk.join("\n"))).unwrap();
    let table = d.path().join("report.tsv");
    let md = d.path().join("report.md");
    let mut argv: Vec<String> = [
        "test",
        "--master",
        master.to_str().unwrap(),
        "--walk",
        walk_path.to_str().unwrap(),
        "--table",
        table.to_str().unwrap(),
        "--markdown",
        md.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    argv.extend(extra.iter().map(|s| s.to_string()));
    let mut stdout = Vec::new();
    run_to(Cli::try_parse_from(argv).unwrap().args, &mut stdout).unwrap();
    (
        String::from_utf8(stdout).unwrap(),
        std::fs::read_to_string(table).unwrap(),
        std::fs::read_to_string(md).unwrap(),
    )
}

#[test]
fn walk_stops_are_classified_by_last_own_value_and_kind() {
    let d = TempDir::new().unwrap();
    let walk = [
        // 先手の最後の手は 2g2f (tree:2, +40)。目標 50 に届かない。
        walk_line("b", "out", 0.02, &["7g7f", "3c3d", "2g2f", "8c8d"]),
        // 先手の最後の手は 7g7f (tree:3, +60)。届く。
        walk_line("b", "rare", 0.03, &["7g7f"]),
        walk_line("b", "own", 0.05, &["7g7f", "8c8d"]),
        // 後手の最後の手は 8c8d (single、後手から +10 は先手から -10)。値の出所が弱い。
        walk_line("w", "max", 0.01, &["2g2f", "8c8d", "7g7f"]),
        walk_line("b", "out", 0.001, &["7g7f", "4c4d", "2g2f"]),
        walk_line("b", "stop", 0.5, &["7g7f"]),
    ];
    let (stdout, table, md) = run_report(&d, &walk, &[]);
    assert_eq!(stdout.trim(), "good=0 unsettled=1 bad=1 gap=0 rejoin=0 shallow=1 weak=1");
    let rows: Vec<Vec<_>> = table.lines().skip(1).map(|l| l.split('\t').collect()).collect();
    let summary: Vec<_> = rows.iter().map(|r| (r[0], r[1], r[4], r[5], r[6], r[7])).collect();
    assert_eq!(
        summary,
        [
            ("unsettled", "b", "0.03", "+60", "tree:3", "届く"),
            ("bad", "b", "0.02", "+40", "tree:2", "届かない"),
            ("shallow", "b", "0.05", "+60", "tree:3", "届く"),
            ("weak", "w", "0.01", "-10", "single", "届く"),
        ]
    );
    // turn は出口の局面の手番、move_number はそこで次に指す手の番号 (手順の手数 + 1)。
    let numbers: Vec<_> = rows.iter().map(|r| (r[1], r[2], r[3])).collect();
    assert_eq!(
        numbers,
        [
            ("b", "w", "2"),
            ("b", "b", "5"),
            ("b", "b", "3"),
            ("w", "w", "4")
        ]
    );
    for line in [
        "| 先手 5 手目 (RAMU) | 0.020 | +40 | tree:2 | 届かない | なし | ▲７六歩 △３四歩 ▲２六歩 △８四歩 |",
        "| 後手 2 手目 (相手) | 0.030 | +60 | tree:3 | 届く | なし | ▲７六歩 |",
        "| 後手 4 手目 (RAMU) | 0.010 | -10 | single | 届く | なし | ▲２六歩 △８四歩 ▲７六歩 |",
    ] {
        assert!(md.contains(line), "{line}\n{md}");
    }
    assert_eq!(rows[0][11], "stop=rare;settle_plies=0;settle_black=+60;settle_end=missing");
    assert_eq!(rows[1][11], "stop=out;settle_plies=0;settle_black=+40;settle_end=missing");
    assert_eq!(rows[2][11], "stop=own");
    assert_eq!(rows[3][11], "stop=max;settle_plies=0;settle_black=-10;settle_end=missing");
    assert!(md.contains("▲７六歩 △３四歩 ▲２六歩 △８四歩 | 0 手 / +40 / missing |"));
    assert!(md.contains("▲２六歩 △８四歩 ▲７六歩 | 0 手 / -10 / missing |"));
    assert!(md.contains("## 1b. 決着していない出口"));
    assert!(md.contains("一本道が ±300 まで通っていない"));
}

#[test]
fn settle_classifies_missing_end_by_the_last_move_value() {
    let d = TempDir::new().unwrap();
    let walk = [walk_line("b", "rare", 0.03, &["7g7f"])];
    for (terminal, class, value) in [
        ("300", "good", "-300"),
        ("-300", "good", "+300"),
        ("299", "unsettled", "-299"),
    ] {
        let rows = [
            master_row(&[], "7g7f", "350", "tree:3"),
            master_row(&["7g7f"], "3c3d", "350", "single"),
            master_row(&["7g7f", "3c3d"], "2g2f", "30", "single"),
            master_row(&["7g7f", "3c3d", "2g2f"], "8c8d", terminal, "single"),
        ];
        let (_, table, _) = run_report_with_rows(&d, &walk, &[], &rows);
        let row = table.lines().nth(1).unwrap();
        assert!(row.starts_with(&format!("{class}\t")), "{row}");
        assert!(row.contains(&format!("settle_plies=3;settle_black={value};settle_end=missing\t")));
    }
}

#[test]
fn settle_uses_last_own_value_when_the_exit_is_missing() {
    let d = TempDir::new().unwrap();
    let walk = [walk_line("b", "out", 0.03, &["7g7f"])];
    let rows = [master_row(&[], "7g7f", "250", "tree:3")];
    let (_, table, _) = run_report_with_rows(&d, &walk, &["--settle-cp", "250"], &rows);
    assert!(table.lines().nth(1).unwrap().starts_with("good\t"), "{table}");
    assert!(table.contains("settle_plies=0;settle_black=+250;settle_end=missing"), "{table}");
}

#[test]
fn settle_counts_a_return_to_the_exit_as_repetition() {
    let d = TempDir::new().unwrap();
    let exit = ["7g7f", "3c3d"];
    let walk = [walk_line("b", "max", 0.03, &exit)];
    let rows = [
        master_row(&[], "7g7f", "60", "tree:3"),
        master_row(&exit, "5i5h", "30", "single"),
        master_row(&["7g7f", "3c3d", "5i5h"], "5a5b", "-25", "single"),
        master_row(&["7g7f", "3c3d", "5i5h", "5a5b"], "5h5i", "20", "single"),
        master_row(&["7g7f", "3c3d", "5i5h", "5a5b", "5h5i"], "5b5a", "-10", "single"),
    ];
    let (_, table, _) = run_report_with_rows(&d, &walk, &["--settle-max-plies", "4"], &rows);
    assert!(table.lines().nth(1).unwrap().starts_with("good\t"), "{table}");
    assert!(
        table.contains("settle_plies=4;settle_black=+10;settle_end=repetition"),
        "{table}"
    );
}

#[test]
fn settle_counts_mate_on_the_last_allowed_move() {
    let d = TempDir::new().unwrap();
    let key = "3lkl3/3p1p3/5G3/9/9/9/9/9/4K4 b G";
    let row = test_row(&[
        ("key", key.to_owned()),
        ("move", "G*5b".to_owned()),
        ("role", "main".to_owned()),
        ("evals", "60".to_owned()),
        ("value_kind", "single".to_owned()),
    ]);
    let master = d.path().join("moves.tsv");
    std::fs::write(&master, format_master(&[row])).unwrap();
    let mut index = MasterIndex::open(&master, &Schema::default()).unwrap();
    let line = settle_line(&mut index, key.to_owned(), None, 1).unwrap();
    assert_eq!((line.moves.len(), line.value_black, line.end.name()), (1, Some(60), "mate"));
    assert_eq!(line.key, child(key, "G*5b").unwrap());
    assert!(line.end.settled(line.value_black, 300));
}

#[test]
fn settle_without_main_uses_best_alt_for_the_side_to_move() {
    let d = TempDir::new().unwrap();
    let exit = ["7g7f"];
    let walk = [walk_line("b", "rare", 0.03, &exit)];
    let row = |mv, evals, role: &str| {
        let mut row = master_row(&exit, mv, evals, "single");
        row.set("role", role.to_owned());
        row
    };
    let rows = [
        master_row(&[], "7g7f", "60", "tree:3"),
        row("8c8d", "20", "alt"),
        row("3c3d", "300", "alt"),
        row("5c5d", "600", "bench"),
        row("4c4d", "900", "record"),
    ];
    let (_, table, _) = run_report_with_rows(&d, &walk, &[], &rows);
    assert!(table.lines().nth(1).unwrap().starts_with("good\t"), "{table}");
    assert!(table.contains("settle_plies=1;settle_black=-300;settle_end=missing"), "{table}");
}

#[test]
fn settle_max_plies_queues_unsettled_at_the_exit_with_miss_or_heavy_priority() {
    let d = TempDir::new().unwrap();
    let walk = [
        walk_line("b", "rare", 0.03, &["7g7f"]),
        walk_line("b", "rare", 0.06, &["2g2f"]),
    ];
    let rows = [
        master_row(&[], "7g7f", "60", "tree:3"),
        master_row(&[], "2g2f", "60", "tree:3"),
        master_row(&["7g7f"], "3c3d", "350", "single"),
        master_row(&["2g2f"], "8c8d", "350", "single"),
        master_row(&["7g7f", "3c3d"], "2g2f", "400", "single"),
        master_row(&["2g2f", "8c8d"], "7g7f", "400", "single"),
    ];
    let enqueue = d.path().join("enqueue.jsonl");
    let (stdout, table, _) = run_report_with_rows(
        &d,
        &walk,
        &[
            "--settle-max-plies",
            "1",
            "--enqueue-out",
            enqueue.to_str().unwrap(),
        ],
        &rows,
    );
    assert!(stdout.starts_with("good=0 unsettled=2 "), "{stdout}");
    assert_eq!(table.matches("settle_plies=1;settle_black=-350;settle_end=max").count(), 2);
    let queued = std::fs::read_to_string(enqueue).unwrap();
    let lines: Vec<Line> = queued.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].root, format!("sfen {} 2", key_after(&["2g2f"])));
    assert_eq!(lines[0].priority, 1);
    assert_eq!(lines[1].root, format!("sfen {} 2", key_after(&["7g7f"])));
    assert_eq!(lines[1].priority, 4);
}

#[test]
fn turn_and_move_number_parity_is_enforced() {
    let item = |key: String, move_number: usize, moves: Option<Vec<String>>| Item {
        class: Class::Rejoin,
        side: "b".into(),
        move_number: Some(move_number),
        weight: 0.1,
        value_black: None,
        kind: String::new(),
        key,
        moves,
        extra: Vec::new(),
        settle: None,
    };
    let black = key_after(&["7g7f", "3c3d"]);
    let white = key_after(&["7g7f"]);
    let path = |moves: &[&str]| Some(moves.iter().map(|m| (*m).to_owned()).collect());
    assert!(check_move_number(&item(black.clone(), 3, path(&["7g7f", "3c3d"]))).is_ok());
    assert!(check_move_number(&item(white.clone(), 2, None)).is_ok());
    // 先手番で偶数、後手番で奇数、手順の長さと合わない番号はどれもエラー。
    assert!(check_move_number(&item(black.clone(), 2, None)).is_err());
    assert!(check_move_number(&item(white.clone(), 3, None)).is_err());
    assert!(check_move_number(&item(black.clone(), 5, path(&["7g7f", "3c3d"]))).is_err());
    // 0 は偶数でも何手目ではない。
    assert!(check_move_number(&item(white.clone(), 0, None)).is_err());
    // 番号が分からない行は、手順も無いときだけ通す。
    let mut unknown = item(white, 2, None);
    unknown.move_number = None;
    assert!(check_move_number(&unknown).is_ok());
    unknown.moves = path(&["7g7f"]);
    assert!(check_move_number(&unknown).is_err());
}

fn rejoin_error(row: &str) -> String {
    let d = TempDir::new().unwrap();
    let rejoin = d.path().join("rejoin.tsv");
    std::fs::write(&rejoin, format!("{REJOIN_HEADER}\n{row}\n")).unwrap();
    let master = d.path().join("moves.tsv");
    std::fs::write(&master, format_master(&[master_row(&[], "7g7f", "", "")])).unwrap();
    let walk = d.path().join("walk.tsv");
    std::fs::write(&walk, format!("{WALK_HEADER}\n")).unwrap();
    let out = d.path().join("out.tsv");
    let argv = [
        "test",
        "--master",
        master.to_str().unwrap(),
        "--walk",
        walk.to_str().unwrap(),
        "--rejoin",
        rejoin.to_str().unwrap(),
        "--table",
        out.to_str().unwrap(),
    ];
    let error = run_to(Cli::try_parse_from(argv).unwrap().args, &mut Vec::new()).unwrap_err();
    assert!(!out.exists());
    format!("{error:#}")
}

#[test]
fn rejoin_ply_zero_is_an_error() {
    let key = key_after(&["2g2f"]);
    let error = rejoin_error(&format!(
        "w\t{key}\t0\t8c8d\tmain\t0\t\t7g7f\t\t3c3d\t\t\t0\t120\t0.1\t10\t2"
    ));
    assert!(error.contains("0 手目"), "{error}");
}

#[test]
fn empty_ply_in_rejoin_and_bridge_tables_is_accepted() {
    let d = TempDir::new().unwrap();
    let walk = [walk_line(
        "b",
        "out",
        0.02,
        &["7g7f", "3c3d", "2g2f", "8c8d"],
    )];
    let on_walk = key_after(&["7g7f", "3c3d"]);
    let off_walk = key_after(&["5g5f"]);
    let bridge = d.path().join("bridge.tsv");
    std::fs::write(
        &bridge,
        format!(
            "key\tply\tside\tmain\tplain\tbridged\tdrop\tbest\tbest_value\tplain_black\tbridged_black\tweight\n\
             {on_walk}\t\tb\t2g2f\t40\t10\t30\t\t\t40\t10\t\n"
        ),
    )
    .unwrap();
    let audit = d.path().join("audit.tsv");
    std::fs::write(
        &audit,
        "key\tply\tside\tsource\tmain\tmain_value\tmain_from\tmain_eval\tbest\tbest_value\tbest_from\tbest_eval\tgap\tmain_value_black\tbest_value_black\n",
    )
    .unwrap();
    let rejoin = d.path().join("rejoin.tsv");
    std::fs::write(
        &rejoin,
        format!(
            "{REJOIN_HEADER}\n\
             b\t{on_walk}\t\t2g2f\tmain\t0\t\t8c8d\t\t2f2e\t\t\t0\t10\t0.1\t10\t2\n\
             w\t{off_walk}\t\t8c8d\tmain\t0\t\t2g2f\t\t3c3d\t\t\t0\t150\t0.1\t10\t1\n"
        ),
    )
    .unwrap();
    let (stdout, table, md) = run_report(
        &d,
        &walk,
        &[
            "--bridge",
            bridge.to_str().unwrap(),
            "--bridge-audit",
            audit.to_str().unwrap(),
            "--rejoin",
            rejoin.to_str().unwrap(),
        ],
    );
    assert!(stdout.contains("gap=1 rejoin=2"), "{stdout}");
    // 手順のある行は手順から番号を求め、手順も ply も無い行は番号を不明と書く。
    assert!(table.contains("gap\tb\tb\t3\t0.02\t"), "{table}");
    assert!(table.contains("rejoin\tb\tb\t3\t0.02\t"), "{table}");
    assert!(table.contains("rejoin\tw\tw\t-\t0.01\t"), "{table}");
    assert!(
        md.contains("| 先手 3 手目 | 0.0200 | ▲２六歩 (0) | △８四歩 → ▲２五歩 (+10) |"),
        "{md}"
    );
    assert!(
        md.contains(
            "| 後手 ? 手目 | 0.0100 | `8c8d` (0) | `2g2f` → `3c3d` (+150) | なし | 手順なし |"
        ),
        "{md}"
    );
}

#[test]
fn rejoin_number_follows_a_longer_path_to_the_same_position() {
    let d = TempDir::new().unwrap();
    // 玉の往復で 4 手余計に指しても同じ局面に着く。表の ply (2) と偶奇が同じなら、手順の長さで数える。
    let long = ["5i5h", "5a5b", "5h5i", "5b5a", "7g7f"];
    let key = key_after(&long);
    assert_eq!(key, key_after(&["7g7f"]));
    let walk = [walk_line("w", "own", 0.02, &long)];
    let rejoin = d.path().join("rejoin.tsv");
    std::fs::write(
        &rejoin,
        format!(
            "{REJOIN_HEADER}\n\
             w\t{key}\t2\t3c3d\tmain\t0\t\t2g2f\t\t8c8d\t\t\t0\t120\t0.1\t10\t2\n"
        ),
    )
    .unwrap();
    let (_, table, md) = run_report(&d, &walk, &["--rejoin", rejoin.to_str().unwrap()]);
    assert!(table.contains("rejoin\tw\tw\t6\t0.02\t"), "{table}");
    assert!(
        md.contains(
            "| 後手 6 手目 | 0.0200 | △３四歩 (0) | ▲２六歩 → △８四歩 (+120) | なし | ▲５八玉 △５二玉 ▲５九玉 △５一玉 ▲７六歩 |"
        ),
        "{md}"
    );
}

#[test]
fn walk_ply_must_equal_the_number_of_moves() {
    let d = TempDir::new().unwrap();
    let master = d.path().join("moves.tsv");
    std::fs::write(&master, format_master(&[master_row(&[], "7g7f", "", "")])).unwrap();
    let walk = d.path().join("walk.tsv");
    let key = key_after(&["7g7f", "3c3d"]);
    std::fs::write(
        &walk,
        format!("{WALK_HEADER}\nb\town\t3\t0.5\t\t\t\t\t{key}\t7g7f 3c3d\t\t\t\t\t\n"),
    )
    .unwrap();
    let out = d.path().join("out.tsv");
    let argv = [
        "test",
        "--master",
        master.to_str().unwrap(),
        "--walk",
        walk.to_str().unwrap(),
        "--table",
        out.to_str().unwrap(),
    ];
    let error = format!(
        "{:#}",
        run_to(Cli::try_parse_from(argv).unwrap().args, &mut Vec::new()).unwrap_err()
    );
    assert!(error.contains("ply 3"), "{error}");
}

const REJOIN_HEADER: &str = "side\tkey\tply\tmove\trole\texpected\tdig\trejoin_move\trejoin\trejoin_main\trejoin_value\tdrop\texpected_black\trejoin_value_black\tweight\tdig_games\trejoin_games";

#[test]
fn rejoin_takes_the_path_from_own_side_walk_rows_and_marks_missing_paths() {
    let d = TempDir::new().unwrap();
    // 同じ局面に相手の側の行と RAMU の側の行が着く。先に読んだ相手の側の行でなく RAMU の側の行の手順を使う。
    let via_other = ["2g2f", "3c3d", "7g7f"];
    let via_own = ["7g7f", "3c3d", "2g2f"];
    let shared = key_after(&via_own);
    assert_eq!(shared, key_after(&via_other));
    let walk = [
        walk_line("b", "out", 0.02, &via_other),
        walk_line("w", "own", 0.02, &via_own),
    ];
    let no_path = key_after(&["5g5f"]);
    let rejoin_key = key_after(&["7g7f", "3c3d", "2g2f", "8c8d", "2f2e"]);
    let rejoin = d.path().join("rejoin.tsv");
    std::fs::write(
        &rejoin,
        format!(
            "{REJOIN_HEADER}\n\
             w\t{shared}\t4\t8c8d\tmain\t0\t\t2f2e\t{rejoin_key}\t8d8e\t\t\t0\t120\t0.1\t10\t2\n\
             w\t{no_path}\t2\t8c8d\tmain\t0\t\t2g2f\t\t3c3d\t\t\t0\t150\t0.1\t10\t1\n"
        ),
    )
    .unwrap();
    let (_, table, md) = run_report(&d, &walk, &["--rejoin", rejoin.to_str().unwrap()]);
    assert!(
        md.contains(
            "| 後手 4 手目 | 0.0200 | △８四歩 (0) | ▲２五歩 → △８五歩 (+120) | なし | ▲７六歩 △３四歩 ▲２六歩 |"
        ),
        "{md}"
    );
    assert!(
        md.contains(
            "| 後手 2 手目 | 0.0100 | `8c8d` (0) | `2g2f` → `3c3d` (+150) | なし | 手順なし |"
        ),
        "{md}"
    );
    let rows: Vec<Vec<_>> = table.lines().skip(1).map(|l| l.split('\t').collect()).collect();
    assert_eq!((rows[0][2], rows[0][3], rows[0][10]), ("w", "4", "7g7f 3c3d 2g2f"));
    assert_eq!((rows[1][2], rows[1][3], rows[1][10]), ("w", "2", "-"));
}

#[test]
fn table_ply_with_the_wrong_parity_is_an_error() {
    let key = key_after(&["2g2f"]);
    let error = rejoin_error(&format!(
        "w\t{key}\t3\t8c8d\tmain\t0\t\t7g7f\t\t3c3d\t\t\t0\t120\t0.1\t10\t2"
    ));
    assert!(error.contains("先手番は奇数、後手番は偶数"), "{error}");
}

#[test]
fn bridge_gaps_and_rejoins_use_walk_weight_and_targets() {
    let d = TempDir::new().unwrap();
    let walk = [
        walk_line("b", "out", 0.02, &["7g7f", "3c3d", "2g2f", "8c8d"]),
        walk_line("b", "out", 0.0005, &["2g2f", "8c8d"]),
    ];
    let gap_key = key_after(&["7g7f", "3c3d"]);
    let light_key = key_after(&["2g2f"]);
    let bridge = d.path().join("bridge.tsv");
    std::fs::write(
        &bridge,
        format!(
            "key\tply\tside\tmain\tplain\tbridged\tdrop\tbest\tbest_value\tplain_black\tbridged_black\tweight\n\
             {gap_key}\t3\tb\t2g2f\t40\t10\t30\t\t\t40\t10\t\n\
             {light_key}\t2\tw\t8c8d\t10\t-200\t210\t\t\t-10\t200\t\n"
        ),
    )
    .unwrap();
    let audit = d.path().join("audit.tsv");
    std::fs::write(
        &audit,
        format!(
            "key\tply\tside\tsource\tmain\tmain_value\tmain_from\tmain_eval\tbest\tbest_value\tbest_from\tbest_eval\tgap\tmain_value_black\tbest_value_black\n\
             {gap_key}\t3\tb\tread\t2g2f\t10\t\t\t6g6f\t60\t\t\t50\t10\t60\n"
        ),
    )
    .unwrap();
    let rejoin = d.path().join("rejoin.tsv");
    std::fs::write(
        &rejoin,
        format!(
            "side\tkey\tply\tmove\trole\texpected\tdig\trejoin_move\trejoin\trejoin_main\trejoin_value\tdrop\texpected_black\trejoin_value_black\tweight\tdig_games\trejoin_games\n\
             w\t{light_key}\t2\t8c8d\tmain\t0\t\t7g7f\t\t3c3d\t\t\t0\t120\t0.1\t10\t2\n\
             w\t{light_key}\t2\t8c8d\tmain\t0\t\t2f2e\t\t\t\t\t0\t30\t0.1\t10\t2\n\
             b\t{gap_key}\t3\t2g2f\tmain\t0\t\t9c9d\t\t\t\t\t0\t10\t0.004\t10\t1\n"
        ),
    )
    .unwrap();
    let args = [
        "--bridge",
        bridge.to_str().unwrap(),
        "--bridge-audit",
        audit.to_str().unwrap(),
        "--rejoin",
        rejoin.to_str().unwrap(),
    ];
    let (stdout, table, md) = run_report(&d, &walk, &args);
    // 軽い局面 (重み 0.0005) の切れ目は落とし、rejoin は来る割合 0.02 の目標割れだけ残す。
    assert!(stdout.contains("gap=1 rejoin=1"), "{stdout}");
    assert!(table.contains("gap\tb\tb\t3\t0.02\t+10\t\t届かない\tなし\t"), "{table}");
    assert!(
        table.contains("main=2g2f;plain_black=40;best=6g6f;best_value_black=60"),
        "{table}"
    );
    assert!(table.contains("rejoin\tw\tw\t2\t0.02\t+120\t\t届かない"), "{table}");
    assert!(
        md.contains(
            "| RAMU が先手 / 先手 3 手目 | 0.020 | ▲２六歩 | +40 → +10 | ▲６六歩 +60 | なし | ▲７六歩 △３四歩 |"
        ),
        "{md}"
    );
    assert!(
        md.contains(
            "| 後手 2 手目 | 0.0200 | △８四歩 (0) | ▲７六歩 → △３四歩 (+120) | なし | ▲２六歩 |"
        ),
        "{md}"
    );
}

#[test]
fn needs_an_output_and_rejects_overwriting_inputs() {
    let d = TempDir::new().unwrap();
    let master = d.path().join("moves.tsv");
    std::fs::write(&master, format_master(&[master_row(&[], "7g7f", "", "")])).unwrap();
    let walk = d.path().join("walk.tsv");
    std::fs::write(&walk, format!("{WALK_HEADER}\n")).unwrap();
    let (m, w) = (master.to_str().unwrap(), walk.to_str().unwrap());
    for extra in [vec![], vec!["--table", m], vec!["--markdown", w]] {
        let argv = ["test", "--master", m, "--walk", w].into_iter().chain(extra);
        assert!(run_to(Cli::try_parse_from(argv).unwrap().args, &mut Vec::new()).is_err());
    }
}

#[test]
fn queued_trees_list_every_box_both_sides_and_skip_depth_zero() {
    let d = TempDir::new().unwrap();
    let exit = ["7g7f", "3c3d", "2g2f", "8c8d"];
    let root = format!("sfen {} 5", key_after(&exit));
    let line = |id: &str, own: &str, depth: usize| {
        serde_json::json!({"id": id, "root": root, "own": own, "depth": depth}).to_string()
    };
    let progress = |trees: &[(&str, &str, bool, bool, u64)]| {
        let trees: Vec<_> = trees
            .iter()
            .map(|(id, own, finished, hold, read)| {
                serde_json::json!({"id": id, "own": own, "priority": 100, "hold": hold,
                    "finished": finished, "read": read, "requested": 92})
            })
            .collect();
        serde_json::json!({"trees": trees, "workers": []}).to_string()
    };
    let make_box = |name: &str, lines: &[String], trees: &[(&str, &str, bool, bool, u64)]| {
        let dir = d.path().join(name);
        std::fs::create_dir_all(dir.join("enqueue")).unwrap();
        std::fs::create_dir_all(dir.join("fetch")).unwrap();
        let text = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
        std::fs::write(dir.join("enqueue/a.jsonl"), &text).unwrap();
        std::fs::write(
            dir.join("enqueue/a.jsonl.receipt"),
            serde_json::json!({"start": 0, "end": text.len(), "lines": lines.len()}).to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("fetch/progress.json"), progress(trees)).unwrap();
        let targets: Vec<_> = trees
            .iter()
            .filter(|(_, _, finished, _, _)| *finished)
            .map(|(id, own, _, _, _)| serde_json::json!({"id":id,"own":own,"state":"DONE"}))
            .collect();
        std::fs::write(
            dir.join("fetch/targets.status.json"),
            serde_json::json!({"targets":targets}).to_string(),
        )
        .unwrap();
        format!("{name}={}", dir.display())
    };
    let mut held = serde_json::from_str::<serde_json::Value>(&line("same", "b", 6)).unwrap();
    held["hold"] = serde_json::json!(true);
    let a = make_box(
        "A",
        &[
            line("both", "bw", 6),
            line("shallow", "b", 0),
            line("same", "b", 6),
        ],
        &[
            ("both", "b", false, false, 40),
            ("both", "w", true, false, 92),
            ("shallow", "b", false, false, 0),
            ("same", "b", true, false, 92),
        ],
    );
    let b = make_box("B", &[held.to_string()], &[("same", "b", false, true, 0)]);
    let walk = [walk_line("b", "out", 0.02, &exit)];
    // rejoin の行は、戻った先の局面を根にした木も受付に数える。
    let rejoin = d.path().join("rejoin.tsv");
    std::fs::write(
        &rejoin,
        format!(
            "{REJOIN_HEADER}\n\
             b\t{}\t3\t2g2f\tmain\t0\t\t8c8d\t{}\t2f2e\t\t\t0\t10\t0.1\t10\t2\n",
            key_after(&exit[..2]),
            key_after(&exit)
        ),
    )
    .unwrap();
    let (_, table, _) = run_report(
        &d,
        &walk,
        &[
            "--box",
            &a,
            "--box",
            &b,
            "--rejoin",
            rejoin.to_str().unwrap(),
        ],
    );
    let queued = "\tboth (A, 40/92); same (A, DONE); same (B, hold)\t";
    for class in ["bad", "rejoin"] {
        let line = table.lines().find(|l| l.starts_with(&format!("{class}\t"))).unwrap();
        assert!(line.contains(queued), "{line}");
    }
    let progress_path = d.path().join("A/fetch/progress.json");
    let master = d.path().join("moves.tsv");
    let walk_path = d.path().join("walk.tsv");
    let argv = [
        "test",
        "--master",
        master.to_str().unwrap(),
        "--walk",
        walk_path.to_str().unwrap(),
        "--box",
        &a,
        "--table",
        progress_path.to_str().unwrap(),
    ];
    assert!(run_to(Cli::try_parse_from(argv).unwrap().args, &mut Vec::new()).is_err());
}

#[test]
fn rejoin_without_weights_is_an_error() {
    let d = TempDir::new().unwrap();
    let rejoin = d.path().join("rejoin.tsv");
    let key = key_after(&["2g2f"]);
    std::fs::write(
        &rejoin,
        format!(
            "side\tkey\tply\tmove\trole\texpected\tdig\trejoin_move\trejoin\trejoin_main\trejoin_value\tdrop\texpected_black\trejoin_value_black\tweight\tdig_games\trejoin_games\n\
             w\t{key}\t2\t8c8d\tmain\t0\t\t3c3d\t\t\t\t\t0\t120\t\t\t\n"
        ),
    )
    .unwrap();
    let master = d.path().join("moves.tsv");
    std::fs::write(&master, format_master(&[master_row(&[], "7g7f", "", "")])).unwrap();
    let walk = d.path().join("walk.tsv");
    std::fs::write(&walk, format!("{WALK_HEADER}\n")).unwrap();
    let out = d.path().join("out.tsv");
    let argv = [
        "test",
        "--master",
        master.to_str().unwrap(),
        "--walk",
        walk.to_str().unwrap(),
        "--rejoin",
        rejoin.to_str().unwrap(),
        "--table",
        out.to_str().unwrap(),
    ];
    let error = format!(
        "{:#}",
        run_to(Cli::try_parse_from(argv).unwrap().args, &mut Vec::new()).unwrap_err()
    );
    assert!(error.contains("--opp-freq"), "{error}");
}

#[test]
fn short_kif_writes_same_square_promotion_drop_and_promoted_pieces() {
    let moves = [
        "7g7f", "3c3d", "8h2b+", "3a2b", "B*4e", "2b3c", "4e3d", "4a3b", "3d2c+", "3c2d", "2c1b",
    ];
    assert_eq!(
        usi_moves_to_short_kif(&moves).unwrap(),
        "▲７六歩 △３四歩 ▲２二角成 △同銀 ▲４五角打 △３三銀 ▲３四角 △３二金 ▲２三角成 △２四銀 ▲１二馬"
    );
}

#[test]
fn gap_at_walk_stop_position_uses_the_row_weight() {
    let d = TempDir::new().unwrap();
    let exit = ["7g7f", "3c3d"];
    let walk = [walk_line("b", "max", 0.02, &exit)];
    let key = key_after(&exit);
    let bridge = d.path().join("bridge.tsv");
    std::fs::write(
        &bridge,
        format!(
            "key\tply\tside\tmain\tplain\tbridged\tdrop\tbest\tbest_value\tplain_black\tbridged_black\tweight\n\
             {key}\t3\tb\t2g2f\t60\t10\t50\t\t\t60\t10\t\n"
        ),
    )
    .unwrap();
    let audit = d.path().join("audit.tsv");
    std::fs::write(
        &audit,
        "key\tply\tside\tsource\tmain\tmain_value\tmain_from\tmain_eval\tbest\tbest_value\tbest_from\tbest_eval\tgap\tmain_value_black\tbest_value_black\n",
    )
    .unwrap();
    let (stdout, table, _) = run_report(
        &d,
        &walk,
        &[
            "--bridge",
            bridge.to_str().unwrap(),
            "--bridge-audit",
            audit.to_str().unwrap(),
        ],
    );
    assert!(stdout.contains("gap=1"), "{stdout}");
    assert!(table.contains("gap\tb\tb\t3\t0.02\t+10\t\t届かない"), "{table}");
}

#[test]
fn bridge_gaps_use_only_the_walk_side_matching_the_turn() {
    let d = TempDir::new().unwrap();
    let path = ["7g7f", "3c3d", "2g2f"];
    let opposite_path = ["2g2f", "3c3d", "7g7f"];
    assert_eq!(key_after(&path), key_after(&opposite_path));
    let key = key_after(&path);
    let bridge = d.path().join("bridge.tsv");
    let audit = d.path().join("audit.tsv");
    std::fs::write(&audit, "key\tgap\n").unwrap();
    for (value, expected_count) in [(82, 1), (80, 1), (79, 0), (41, 0)] {
        std::fs::write(
            &bridge,
            format!(
                "key\tply\tside\tmain\tplain_black\tbridged_black\n{key}\t4\tw\t8c8d\t0\t{value}\n"
            ),
        )
        .unwrap();
        let walk = [
            walk_line("b", "out", 0.0095, &opposite_path),
            walk_line("w", "own", 0.003, &path),
        ];
        let (_, table, md) = run_report(
            &d,
            &walk,
            &[
                "--bridge",
                bridge.to_str().unwrap(),
                "--bridge-audit",
                audit.to_str().unwrap(),
            ],
        );
        let gaps: Vec<Vec<_>> = table
            .lines()
            .filter(|l| l.starts_with("gap\t"))
            .map(|l| l.split('\t').collect())
            .collect();
        assert_eq!(gaps.len(), expected_count, "{table}");
        if expected_count == 1 {
            assert_eq!((gaps[0][1], gaps[0][2], gaps[0][4]), ("w", "w", "0.003"));
            assert_eq!(gaps[0][10], path.join(" "));
            assert!(md.contains("RAMU が後手 / 後手 4 手目"), "{md}");
        }
    }
    std::fs::write(&audit, format!("key\tgap\tbest\tbest_value_black\n{key}\t50\t3c3d\t10\n"))
        .unwrap();
    let walk = [
        walk_line("b", "out", 0.02, &path),
        walk_line("w", "own", 0.0006, &path),
    ];
    let (_, table, _) = run_report(
        &d,
        &walk,
        &[
            "--bridge",
            bridge.to_str().unwrap(),
            "--bridge-audit",
            audit.to_str().unwrap(),
        ],
    );
    assert!(!table.lines().any(|l| l.starts_with("gap\t")), "{table}");
}

#[test]
fn bridge_from_audit_uses_the_turn_side_graph_with_opponent_records() {
    use super::super::audit::{AuditArgs, Fill, evaluate_for_side, opponent_replies, run};
    use super::super::master_table::parse_master_rows;
    use crate::book_backprop::DrawValues;

    #[derive(Parser)]
    struct AuditCli {
        #[command(flatten)]
        args: AuditArgs,
    }

    let d = TempDir::new().unwrap();
    let path = ["7g7f"];
    let key = key_after(&path);
    let mut record = master_row(&["7g7f", "3c3d", "2g2f"], "3a4b", "200", "single");
    record.set("role", "record".into());
    let rows = [
        master_row(&path, "3c3d", "50", "single"),
        master_row(&["7g7f", "3c3d", "2g2f"], "8c8d", "-100", "single"),
        record,
    ];
    let (positions, _) = parse_master_rows(format_master(&rows).as_bytes()).unwrap();
    let records = opponent_replies(&rows, &["record"]).unwrap();
    let draws = DrawValues {
        black: -70,
        white: 70,
    };
    let position = positions.keys().position(|k| k == &key).unwrap();
    let black = evaluate_for_side(&positions, &records, draws, Fill::Bridge, "b").unwrap();
    let white = evaluate_for_side(&positions, &records, draws, Fill::Bridge, "w").unwrap();
    assert_eq!(black.values[position][0], Some(50));
    assert_eq!(white.values[position][0], Some(-100));
    let master = d.path().join("moves.tsv");
    let audit = d.path().join("audit.tsv");
    let bridge = d.path().join("bridge.tsv");
    std::fs::write(&master, format_master(&rows)).unwrap();
    let args = AuditCli::try_parse_from([
        "test",
        "--master",
        master.to_str().unwrap(),
        "--table",
        audit.to_str().unwrap(),
        "--bridge-table",
        bridge.to_str().unwrap(),
        "--bridge-replies",
        "--opp-records",
        "--threads",
        "2",
    ])
    .unwrap()
    .args;
    run(args).unwrap();
    let text = std::fs::read_to_string(&bridge).unwrap();
    assert!(text.contains(&format!("{key}\t\tw\t3c3d\t50\t-100\t150")), "{text}");
    let walk = [
        walk_line("b", "stop", 0.02, &path),
        walk_line("w", "stop", 0.003, &path),
    ];
    for (target, expected_count) in [("80", 1), ("101", 0)] {
        let (_, table, _) = run_report_with_rows(
            &d,
            &walk,
            &[
                "--bridge",
                bridge.to_str().unwrap(),
                "--bridge-audit",
                audit.to_str().unwrap(),
                "--target-black",
                "150",
                "--target-white",
                target,
            ],
            &rows,
        );
        let gaps: Vec<_> = table.lines().filter(|l| l.starts_with("gap\t")).collect();
        assert_eq!(gaps.len(), expected_count, "{table}");
        if expected_count == 1 {
            assert!(gaps[0].starts_with("gap\tw\tw\t2\t0.003\t+100\t\t届かない"), "{table}");
        }
    }
}

fn read_queue(path: &Path) -> Vec<Line> {
    let mut lines = Vec::new();
    super::super::targets::read_checked_lines(path, |line, _, _| {
        lines.push(line);
        Ok(())
    })
    .unwrap();
    lines
}

#[test]
fn enqueue_bad_continuation_moves_to_missing_root_only_when_unsettled() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let exit = ["7g7f"];
    let terminal = ["7g7f", "3c3d", "2g2f"];
    let exit_key = key_after(&exit);
    let terminal_key = key_after(&terminal);
    let walk = [walk_line("b", "out", 0.02, &exit)];
    for (value, has_record, moved) in [
        ("299", false, true),
        ("300", false, false),
        ("-300", false, false),
        ("299", true, false),
    ] {
        let mut rows = vec![
            master_row(&[], "7g7f", "40", "tree:3"),
            master_row(&exit, "3c3d", "20", "single"),
            master_row(&["7g7f", "3c3d"], "2g2f", value, "single"),
        ];
        if has_record {
            let mut record = master_row(&terminal, "8c8d", "10", "single");
            record.set("role", "record".into());
            rows.push(record);
        }
        let (stdout, table, md) =
            run_report_with_rows(&d, &walk, &["--enqueue-out", out.to_str().unwrap()], &rows);
        let (key, number) = if moved {
            (&terminal_key, 4)
        } else {
            (&exit_key, 2)
        };
        let lines = read_queue(&out);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].root, format!("sfen {key} {number}"));
        assert_eq!(lines[0].id, queue_id("exit-report", number, key, "b"));
        assert_eq!(lines[0].own, "b");
        assert_eq!(lines[0].priority, 4);
        assert_eq!(lines[0].memo.as_ref().unwrap().contains("元データの続き 2 手の先"), moved);
        assert!(stdout.contains(&format!("続きへ移した行={}", usize::from(moved))), "{stdout}");
        let row: Vec<_> = table.lines().nth(1).unwrap().split('\t').collect();
        assert_eq!(&row[12..], ["ok", &lines[0].id]);
        let black = fmt_value(Some(value.parse().unwrap()));
        assert_eq!((row[0], row[3], row[9], row[10]), ("bad", "2", exit_key.as_str(), "7g7f"));
        assert_eq!(
            row[11],
            format!("stop=out;settle_plies=2;settle_black={black};settle_end=missing")
        );
        assert!(md.contains(&format!("▲７六歩 | 2 手 / {black} / missing |")), "{md}");
    }
}

#[test]
fn enqueue_weak_continuation_uses_exit_value_and_memo_keeps_its_exit_number() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let terminal = ["7g7f", "3c3d", "2g2f"];
    let walk = [
        walk_line("b", "out", 0.02, &terminal[..1]),
        walk_line("b", "out", 0.02, &terminal),
    ];
    let rows = [
        master_row(&[], "7g7f", "60", "single"),
        master_row(&terminal[..1], "3c3d", "20", "single"),
        master_row(&terminal[..2], "2g2f", "20", "tree:2"),
    ];
    let options = ["--enqueue-out", out.to_str().unwrap()];
    let (_, table, _) = run_report_with_rows(&d, &walk[..1], &options, &rows);
    assert!(table.lines().nth(1).unwrap().starts_with("weak\t"));
    let root = format!("sfen {} 4", key_after(&terminal));
    let lines = read_queue(&out);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].root, root);
    assert_eq!(lines[0].priority, 7);

    let (stdout, table, _) = run_report_with_rows(&d, &walk, &options, &rows);
    let lines = read_queue(&out);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].root, root);
    assert!(stdout.contains("まとめた行=1"), "{stdout}");
    let memo = lines[0].memo.as_deref().unwrap();
    assert!(memo.starts_with("bad 4手目 "), "{memo}");
    assert!(memo.contains("元データの続き 2 手の先 (2 手目の出口から)"), "{memo}");
    assert!(memo.contains("path=[bad@4=ok; weak@2=ok]"), "{memo}");
    assert_eq!(table.matches(&format!("\tok\t{}\n", lines[0].id)).count(), 2);
    assert!(stdout.contains("path_not_ok=0"), "{stdout}");
    for (name, path) in [("master", &d.path().join("moves.tsv")), ("enqueue", &out)] {
        let digest = super::super::journal::digest(path).unwrap();
        assert!(stdout.contains(&format!("{name}_sha256={digest}")), "{stdout}");
    }
}

#[test]
fn enqueue_continuation_checks_box_at_both_roots_and_reports_without_enqueue() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let terminal = ["7g7f", "3c3d"];
    let walk = [walk_line("b", "out", 0.02, &terminal[..1])];
    let rows = [
        master_row(&[], "7g7f", "40", "tree:3"),
        master_row(&terminal[..1], "3c3d", "20", "single"),
    ];
    let box_dir = d.path().join("box");
    std::fs::create_dir_all(box_dir.join("enqueue")).unwrap();
    std::fs::create_dir_all(box_dir.join("fetch")).unwrap();
    let box_spec = format!("A={}", box_dir.display());
    let exit_root = format!("sfen {} 2", key_after(&terminal[..1]));
    let continuation_root = format!("sfen {} 3", key_after(&terminal));
    for (finished, include_continuation, expected_excluded) in [
        (false, true, 1),
        (false, false, 1),
        (true, false, 0),
        (true, true, 1),
    ] {
        let exit = serde_json::json!({"id": "exit", "root": exit_root, "own": "b", "depth": 2});
        let mut text = format!("{exit}\n");
        let mut progress = vec![serde_json::json!({
            "id": "exit", "own": "b", "priority": 100, "hold": false,
            "finished": finished, "read": if finished { 92 } else { 40 }, "requested": 92
        })];
        if include_continuation {
            let continuation = serde_json::json!({
                "id": "next", "root": continuation_root, "own": "b", "depth": 2
            });
            text.push_str(&format!("{continuation}\n"));
            progress.push(serde_json::json!({
                "id": "next", "own": "b", "priority": 100, "hold": false,
                "finished": true, "read": 92, "requested": 92
            }));
        }
        std::fs::write(box_dir.join("enqueue/a.jsonl"), &text).unwrap();
        std::fs::write(
            box_dir.join("enqueue/a.jsonl.receipt"),
            serde_json::json!({"start": 0, "end": text.len(), "lines": progress.len()}).to_string(),
        )
        .unwrap();
        std::fs::write(
            box_dir.join("fetch/progress.json"),
            serde_json::json!({"trees": progress, "workers": []}).to_string(),
        )
        .unwrap();

        let targets: Vec<_> = progress
            .iter()
            .filter(|p| p["finished"] == true)
            .map(|p| serde_json::json!({"id":p["id"],"own":p["own"],"state":"DONE"}))
            .collect();
        std::fs::write(
            box_dir.join("fetch/targets.status.json"),
            serde_json::json!({"targets":targets}).to_string(),
        )
        .unwrap();

        let (_, table, md) = run_report_with_rows(&d, &walk, &["--queue-box", &box_spec], &rows);
        let state = if finished { "DONE" } else { "40/92" };
        let mut queued = format!("exit (A, {state})");
        if include_continuation {
            queued.push_str("; next (A, DONE) (続きの先)");
        }
        assert!(table.contains(&format!("\t{queued}\t")), "{table}");
        assert!(md.contains(&format!("| {queued} |")), "{md}");
        assert!(md.contains("出口と続きの先の両方を照合"), "{md}");

        let (stdout, with_enqueue_table, with_enqueue_md) = run_report_with_rows(
            &d,
            &walk,
            &[
                "--queue-box",
                &box_spec,
                "--enqueue-out",
                out.to_str().unwrap(),
            ],
            &rows,
        );
        assert!(table.ends_with("\tok\tnot-requested\n"));
        if expected_excluded == 1 {
            let reason = if finished {
                "covered:next(A,DONE)"
            } else {
                "covered:exit(A,40/92)"
            };
            assert!(with_enqueue_table.ends_with(&format!("\tok\t{reason}\n")));
        }
        assert_eq!(with_enqueue_md, md);
        assert!(stdout.contains(&format!("照合除外={expected_excluded} ")), "{stdout}");
        let lines = read_queue(&out);
        assert_eq!(lines.len(), 1 - expected_excluded);
        if let [line] = lines.as_slice() {
            assert_eq!(line.root, continuation_root);
        }
    }
}

#[test]
fn enqueue_continuations_merge_and_match_receipts_at_the_new_root() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let exclude = d.path().join("existing.jsonl");
    let walk = [
        walk_line("b", "out", 0.03, &["7g7f"]),
        walk_line("b", "rare", 0.02, &["2g2f"]),
    ];
    let rows = [
        master_row(&[], "7g7f", "20", "tree:2"),
        master_row(&[], "2g2f", "20", "tree:2"),
        master_row(&["7g7f"], "3c3d", "20", "single"),
        master_row(&["2g2f"], "3c3d", "20", "single"),
        master_row(&["7g7f", "3c3d"], "2g2f", "20", "single"),
        master_row(&["2g2f", "3c3d"], "7g7f", "20", "single"),
    ];
    let root = format!("sfen {} 4", key_after(&["7g7f", "3c3d", "2g2f"]));
    for (existing_root, own, expected_excluded) in [
        (format!("sfen {} 2", key_after(&["7g7f"])), "b", 1),
        (root.clone(), "w", 0),
        (root.clone(), "b", 2),
    ] {
        let existing = serde_json::json!({
            "id": "existing", "root": existing_root, "own": own, "depth": 2
        });
        std::fs::write(&exclude, format!("{existing}\n")).unwrap();
        let (stdout, table, _) = run_report_with_rows(
            &d,
            &walk,
            &[
                "--enqueue-out",
                out.to_str().unwrap(),
                "--exclude-queue",
                exclude.to_str().unwrap(),
            ],
            &rows,
        );
        let lines = read_queue(&out);
        assert_eq!(lines.len(), usize::from(expected_excluded < 2));
        assert!(stdout.contains(&format!("照合除外={expected_excluded} ")), "{stdout}");
        assert!(stdout.contains("続きへ移した行=2"), "{stdout}");
        if let [line] = lines.as_slice() {
            assert_eq!(line.root, root);
            assert!(
                line.memo
                    .as_ref()
                    .unwrap()
                    .contains("元データの続き 2 手の先 (2 手目の出口から)")
            );
            assert!(stdout.contains(&format!("まとめた行={}", 1 - expected_excluded)), "{stdout}");
        }
        assert_eq!(table.matches("existing (exclude-queue)").count(), expected_excluded);
        assert_eq!(
            table.matches("existing (exclude-queue) (続きの先)").count(),
            if expected_excluded == 2 { 2 } else { 0 }
        );
        if expected_excluded == 1 {
            let (_, table, _) = run_report_with_rows(
                &d,
                &walk[..1],
                &[
                    "--enqueue-out",
                    out.to_str().unwrap(),
                    "--exclude-queue",
                    exclude.to_str().unwrap(),
                ],
                &rows,
            );
            assert!(read_queue(&out).is_empty());
            assert!(table.contains("\texisting (exclude-queue)\t"));
        }
    }
}

#[test]
fn enqueue_exclude_through_filters_only_the_exit_path() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let keys = d.path().join("fixed.keys");
    let reverse = d.path().join("reverse.keys");
    let terminal = ["7g7f", "3c3d", "2g2f"];
    let beyond = ["7g7f", "3c3d", "2g2f", "8c8d", "2f2e"];
    let continued_root = format!("sfen {} 6", key_after(&beyond));
    let other = ["2g2f"];
    let walk = [
        walk_line("b", "out", 0.04, &beyond[..4]),
        walk_line("w", "own", 0.03, &terminal),
        walk_line("b", "out", 0.02, &other),
        walk_line("b", "out", 0.01, &terminal[..1]),
    ];
    let rows = [
        master_row(&[], "7g7f", "60", "tree:3"),
        master_row(&terminal[..1], "3c3d", "40", "tree:2"),
        master_row(&terminal[..2], "2g2f", "40", "tree:2"),
        master_row(&terminal, "8c8d", "-90", "single"),
        master_row(&beyond[..4], "2f2e", "40", "single"),
    ];
    let out_arg = ["--enqueue-out", out.to_str().unwrap()];
    let (baseline_stdout, baseline_table, baseline_md) =
        run_report_with_rows(&d, &walk, &out_arg, &rows);
    let baseline_queue = read_queue(&out);
    let baseline_bytes = std::fs::read(&out).unwrap();
    assert_eq!(baseline_queue.len(), 3);
    let continued: Vec<_> =
        baseline_queue.iter().filter(|line| line.root == continued_root).collect();
    assert_eq!(continued.len(), 1);
    let memo = continued[0].memo.as_deref().unwrap();
    assert!(memo.contains("bad@5=ok"), "{memo}");
    assert!(memo.contains("unsettled@2=ok"), "{memo}");
    assert!(baseline_stdout.contains("まとめた行=1"));
    assert!(!baseline_stdout.contains("excluded_through="));
    assert!(!baseline_md.contains("excluded-through"));

    let key = key_after(&terminal);
    let flipped = rshogi_book::flipped_key(&format!("{key} 4")).unwrap();
    std::fs::write(&keys, format!("\n  # 固定する区間\n  {key}  \n \t\n")).unwrap();
    std::fs::write(&reverse, format!("{}\n", strip_ply(&flipped))).unwrap();
    let reverse_arg = ["--exclude-through", reverse.to_str().unwrap()];
    let (stdout, table, md) =
        run_report_with_rows(&d, &walk, &[out_arg, reverse_arg].concat(), &rows);
    assert_eq!(table, baseline_table);
    assert_eq!(std::fs::read(&out).unwrap(), baseline_bytes);
    assert_eq!(stdout, format!("{} excluded_through=0\n", baseline_stdout.trim_end()));
    let summary = "通過局面で除外した行 (excluded-through) は 0 件です。\n\n";
    assert_eq!(md.matches(summary).count(), 1);
    let md_without_summary = md.replacen(summary, "", 1);
    assert_eq!(md_without_summary.lines().count(), baseline_md.lines().count());
    for (actual, baseline) in md_without_summary.lines().zip(baseline_md.lines()) {
        if baseline.starts_with('|') {
            let (body, enqueue) = actual.strip_suffix('|').unwrap().rsplit_once('|').unwrap();
            assert_eq!(format!("{body}|"), baseline);
            assert_ne!(enqueue.trim(), "excluded-through");
        } else {
            assert_eq!(actual, baseline);
        }
    }

    let fixed_arg = ["--exclude-through", keys.to_str().unwrap()];
    let (stdout, table, md) =
        run_report_with_rows(&d, &walk, &[out_arg, reverse_arg, fixed_arg].concat(), &rows);
    // 続きで初めて固定局面を通る出口は残り、同じ根に統合される別の出口だけが外れる。
    let queue = read_queue(&out);
    assert_eq!(queue.len(), 2);
    let continued = queue.iter().find(|line| line.root == continued_root).unwrap();
    let memo = continued.memo.as_deref().unwrap();
    assert!(memo.ends_with(" path=[unsettled@2=ok]"), "{memo}");
    assert!(queue.iter().any(|line| line.root == format!("sfen {} 2", key_after(&other))));
    assert_eq!(baseline_table.lines().count(), table.lines().count());
    for (before, after) in baseline_table.lines().zip(table.lines()).skip(1) {
        let before = before.rsplit_once('\t').unwrap().0;
        let (after, disposition) = after.rsplit_once('\t').unwrap();
        assert_eq!(before, after);
        let moves = after.split('\t').nth(10).unwrap();
        assert_eq!(
            disposition == "excluded-through",
            matches!(moves, "7g7f 3c3d 2g2f" | "7g7f 3c3d 2g2f 8c8d")
        );
    }
    assert!(stdout.ends_with(" excluded_through=2\n"));
    assert!(md.contains("通過局面で除外した行 (excluded-through) は 2 件です。"));
    assert_eq!(md.matches("| excluded-through |").count(), 2);

    let (stdout, table, md) = run_report_with_rows(&d, &walk, &fixed_arg, &rows);
    assert!(stdout.ends_with("\nexcluded_through=2\n"));
    assert_eq!(table.matches("\texcluded-through\n").count(), 2);
    assert_eq!(md.matches("| excluded-through |").count(), 2);

    let queue_path = d.path().join("existing.jsonl");
    let existing = serde_json::json!({
        "id": "existing", "root": format!("sfen {key} 4"), "own": "w", "depth": 2
    });
    std::fs::write(&queue_path, format!("{existing}\n")).unwrap();
    let (stdout, table, _) = run_report_with_rows(
        &d,
        &walk,
        &[
            out_arg,
            fixed_arg,
            ["--exclude-queue", queue_path.to_str().unwrap()],
        ]
        .concat(),
        &rows,
    );
    assert!(stdout.contains("照合除外=1 "));
    assert!(stdout.ends_with(" excluded_through=1\n"));
    assert!(table.contains("\tcovered:existing(exclude-queue,listed)\n"));
    assert_eq!(table.matches("\texcluded-through\n").count(), 1);
}

#[test]
fn exclude_through_rejects_invalid_keys_before_writing_outputs() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let keys = d.path().join("fixed.keys");
    let table_dir = d.path().join("new-output");
    let table = table_dir.join("report.tsv");
    let md = d.path().join("report.md");
    let mut args = enqueue_args(&out);
    args.master = d.path().join("moves.tsv");
    args.walk = d.path().join("walk.tsv");
    args.table = Some(table.clone());
    args.markdown = Some(md.clone());
    args.exclude_through.push(keys.clone());
    for bad in [
        format!("{HIRATE_KEY} 1"),
        "not-sfen b -".to_owned(),
        HIRATE_KEY.replace(" b ", " x "),
        HIRATE_KEY.replace(" -", " 2"),
    ] {
        std::fs::write(&keys, format!("# 固定する局面\n\n{bad}\n")).unwrap();
        let err = excluded_through_keys(&args.exclude_through).unwrap_err();
        assert!(err.to_string().contains(&format!("{}:3", keys.display())));
    }
    let mut stdout = Vec::new();
    let err = run_to(args, &mut stdout).unwrap_err();
    assert!(err.to_string().contains(&format!("{}:3", keys.display())));
    assert!(stdout.is_empty());
    assert!([table, md, out].iter().all(|path| !path.exists()));
    assert!(!table_dir.exists());
}

#[test]
fn enqueue_walk_classes_use_exit_roots_and_three_priority_levels() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let paths: [&[&str]; 4] = [
        &["7g7f", "3c3d", "2g2f", "8c8d"],
        &["7g7f", "8c8d"],
        &["2g2f", "8c8d", "7g7f"],
        &["7g7f"],
    ];
    let walk = [
        walk_line("b", "out", 0.02, paths[0]),
        walk_line("b", "own", 0.05, paths[1]),
        walk_line("w", "max", 0.01, paths[2]),
        walk_line("b", "rare", 0.1, paths[3]),
    ];
    let (stdout, _, _) = run_report(
        &d,
        &walk,
        &[
            "--enqueue-out",
            out.to_str().unwrap(),
            "--tag",
            "edition",
            "--depth",
            "4",
            "--opp-n",
            "3",
            "--settle-cp",
            "60",
            "--stop",
            "150",
            "--rounds",
            "2",
            "--draw-black",
            "-70",
            "--draw-white",
            "70",
        ],
    );
    assert!(stdout.starts_with("good=1 unsettled=0 "), "{stdout}");
    assert!(stdout.contains("出力=3 照合除外=0 手数なし=0 まとめた行=0"), "{stdout}");
    let lines = read_queue(&out);
    assert_eq!(lines.len(), 3);
    for (i, line) in lines.iter().enumerate() {
        let own = if i == 2 { "w" } else { "b" };
        let key = key_after(paths[i]);
        let number = paths[i].len() + 1;
        assert_eq!(line.root, format!("sfen {key} {number}"));
        assert_eq!(line.id, queue_id("edition", number, &key, own));
        assert_eq!(line.own, own);
        assert!(line.game.is_empty());
        assert_eq!(line.depth, 4);
        assert_eq!(line.opts.opp_n, Some(3));
        assert_eq!(line.opts.stop, Some(150));
        assert_eq!(line.opts.rounds, Some(2));
        assert_eq!(line.opts.draw_black, Some(-70));
        assert_eq!(line.opts.draw_white, Some(70));
    }
    assert_eq!(lines.iter().map(|l| l.priority).collect::<Vec<_>>(), [4, 1, 7]);
    assert!(lines[1].memo.as_ref().unwrap().contains("先手値=+60"));
}

fn write_rejoins(d: &TempDir) -> PathBuf {
    let path = d.path().join("rejoin.tsv");
    let start = key_after(&["7g7f", "3c3d"]);
    let missing = key_after(&["5g5f"]);
    std::fs::write(
        &path,
        format!(
            "{REJOIN_HEADER}\n\
         b\t{start}\t3\t2g2f\tmain\t0\t\t8c8d\t\t2f2e\t\t\t0\t10\t0.1\t10\t2\n\
         b\t{start}\t3\t2g2f\tmain\t0\t\t4c4d\t\t2f2e\t\t\t0\t10\t0.1\t10\t1\n\
         w\t{missing}\t\t8c8d\tmain\t0\t\t2g2f\t\t3c3d\t\t\t0\t150\t0.1\t10\t1\n"
        ),
    )
    .unwrap();
    path
}

#[test]
fn enqueue_rejoin_uses_dig_and_merges_reply_games_and_counts_missing_numbers() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let rejoin = write_rejoins(&d);
    let (stdout, table, _) = run_report(
        &d,
        &[],
        &[
            "--rejoin",
            rejoin.to_str().unwrap(),
            "--enqueue-out",
            out.to_str().unwrap(),
        ],
    );
    assert!(stdout.contains("出力=1 照合除外=0 手数なし=1 まとめた行=1"), "{stdout}");
    let lines = read_queue(&out);
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert_eq!(line.root, format!("sfen {} 4", key_after(&["7g7f", "3c3d", "2g2f"])));
    assert_eq!(line.own, "b");
    assert_eq!(line.game, [vec!["8c8d"], vec!["4c4d"]]);
    assert_eq!(line.priority, 4);
    assert!(
        line.memo
            .as_ref()
            .unwrap()
            .contains("重み=0.02000 (2件) path=[rejoin@3=-; rejoin@3=-]")
    );
    assert!(table.contains("\t-\tno-move-number\n"));
    assert!(stdout.contains("path_not_ok=1"));
}

#[test]
fn enqueue_heavier_rejoin_without_number_merges_into_the_same_dig_root() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let rejoin = d.path().join("rejoin.tsv");
    let start = key_after(&["7g7f", "3c3d"]);
    std::fs::write(
        &rejoin,
        format!(
            "{REJOIN_HEADER}\n\
         b\t{start}\t3\t2g2f\tmain\t0\t\t8c8d\t\t2f2e\t\t\t0\t10\t0.1\t10\t2\n\
         b\t{start}\t\t2g2f\tmain\t0\t\t4c4d\t\t2f2e\t\t\t0\t10\t0.1\t10\t3\n"
        ),
    )
    .unwrap();
    let (stdout, _, _) = run_report(
        &d,
        &[],
        &[
            "--rejoin",
            rejoin.to_str().unwrap(),
            "--enqueue-out",
            out.to_str().unwrap(),
        ],
    );
    assert!(stdout.contains("出力=1 照合除外=0 手数なし=0 まとめた行=1"), "{stdout}");
    let lines = read_queue(&out);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].root, format!("sfen {} 4", key_after(&["7g7f", "3c3d", "2g2f"])));
    assert_eq!(lines[0].game, [vec!["4c4d"], vec!["8c8d"]]);
    assert_eq!(lines[0].priority, 4);
}

#[test]
fn enqueue_gap_uses_main_even_when_best_differs_and_merges_with_shallow() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let bridge = d.path().join("bridge.tsv");
    let audit = d.path().join("audit.tsv");
    let key = key_after(&["7g7f", "3c3d"]);
    std::fs::write(
        &bridge,
        format!("key\tply\tside\tmain\tplain_black\tbridged_black\n{key}\t3\tb\t2g2f\t60\t60\n"),
    )
    .unwrap();
    std::fs::write(&audit, format!("key\tgap\tbest\tbest_value_black\n{key}\t50\t5g5f\t110\n"))
        .unwrap();
    let (stdout, _, _) = run_report(
        &d,
        &[walk_line("b", "own", 0.05, &["7g7f", "3c3d"])],
        &[
            "--bridge",
            bridge.to_str().unwrap(),
            "--bridge-audit",
            audit.to_str().unwrap(),
            "--enqueue-out",
            out.to_str().unwrap(),
        ],
    );
    assert!(stdout.contains("出力=1 照合除外=0 手数なし=0 まとめた行=1"), "{stdout}");
    let lines = read_queue(&out);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].root, format!("sfen {key} 3"));
    assert_eq!(lines[0].own, "b");
    assert_eq!(lines[0].game, [vec!["2g2f"]]);
    assert_eq!(lines[0].priority, 1);
    assert!(lines[0].memo.as_ref().unwrap().starts_with("gap "));
}

fn enqueue_args(out: &Path) -> ExitReportArgs {
    Cli::try_parse_from([
        "test",
        "--master",
        "master.tsv",
        "--walk",
        "walk.tsv",
        "--enqueue-out",
        out.to_str().unwrap(),
    ])
    .unwrap()
    .args
}

#[test]
fn enqueue_merge_keeps_earlier_class_heavier_representative_and_minimum_priority() {
    let d = TempDir::new().unwrap();
    let args = enqueue_args(&d.path().join("out.jsonl"));
    let item = |class, weight, value_black, mv: &str| Item {
        class,
        side: "b".into(),
        move_number: Some(1),
        weight,
        value_black,
        kind: "tree:2".into(),
        key: HIRATE_KEY.into(),
        moves: Some(Vec::new()),
        extra: vec![("main", mv.into())],
        settle: None,
    };
    let items = [
        item(Class::Shallow, 0.1, Some(60), ""),
        item(Class::Gap, 0.01, Some(60), "2g2f"),
        item(Class::Gap, 0.02, Some(10), "7g7f"),
        item(Class::Gap, 0.01, Some(60), "2g2f"),
    ];
    let paths = ["ok", "pos:1", "ok", "move:2"].map(str::to_owned);
    let EnqueuePlan {
        lines,
        counts,
        dispositions,
    } = queue_lines(&items, &paths, &args, &[], &Excluded::new(), &BTreeSet::new(), &[]).unwrap();
    assert!(dispositions.iter().all(|id| id == &lines[0].id));
    assert_eq!(counts.path_not_ok, 1);
    assert_eq!(lines.len(), 1);
    assert_eq!(counts.merged, 3);
    assert_eq!(lines[0].priority, 1);
    assert_eq!(lines[0].game, [vec!["7g7f"], vec!["2g2f"]]);
    assert_eq!(
        lines[0].memo.as_deref(),
        Some(
            "gap 1手目 先手値=+10 出所=tree:2 重み=0.02000 (4件) path=[gap@1=ok; gap@1=pos:1; gap@1=move:2; shallow@1=ok]"
        )
    );
}

#[test]
fn enqueue_exclude_queue_counts_game_positions_and_dig_roots() {
    let d = TempDir::new().unwrap();
    let rejoin = write_rejoins(&d);
    let out = d.path().join("plan.jsonl");
    let exclude = d.path().join("existing.jsonl");
    std::fs::write(&exclude, format!("{}\n{}\n", serde_json::json!({
        "id": "dig", "root": format!("sfen {} 4", key_after(&["7g7f", "3c3d", "2g2f"])), "own": "b", "depth": 2
    }), serde_json::json!({
        "id": "game", "root": format!("sfen {HIRATE_KEY} 1"), "own": "bw", "depth": 6,
        "game": [["2g2f", "8c8d", "7g7f"]]
    }))).unwrap();
    let walk = [walk_line("w", "max", 0.01, &["2g2f", "8c8d", "7g7f"])];
    let (stdout, table, _) = run_report(
        &d,
        &walk,
        &[
            "--rejoin",
            rejoin.to_str().unwrap(),
            "--enqueue-out",
            out.to_str().unwrap(),
            "--exclude-queue",
            exclude.to_str().unwrap(),
        ],
    );
    assert!(stdout.contains("出力=0 照合除外=3 手数なし=1 まとめた行=0"), "{stdout}");
    assert!(read_queue(&out).is_empty());
    assert!(
        table
            .lines()
            .filter(|l| l.starts_with("rejoin\tb"))
            .all(|l| l.contains("dig (exclude-queue)")),
        "{table}"
    );
}

#[test]
fn enqueue_box_matches_dig_root_and_own_but_ignores_depth_zero() {
    let d = TempDir::new().unwrap();
    let rejoin = write_rejoins(&d);
    let out = d.path().join("plan.jsonl");
    let box_dir = d.path().join("box");
    std::fs::create_dir_all(box_dir.join("enqueue")).unwrap();
    let root = format!("sfen {} 4", key_after(&["7g7f", "3c3d", "2g2f"]));
    for (own, depth, expected) in [("w", 6, 1), ("b", 0, 1), ("bw", 6, 0)] {
        let text =
            format!("{}\n", serde_json::json!({"id":"dig", "root":root, "own":own, "depth":depth}));
        std::fs::write(box_dir.join("enqueue/a.jsonl"), &text).unwrap();
        std::fs::write(
            box_dir.join("enqueue/a.jsonl.receipt"),
            serde_json::json!({"start":0,"end":text.len(),"lines":1}).to_string(),
        )
        .unwrap();
        let (stdout, table, md) = run_report(
            &d,
            &[],
            &[
                "--rejoin",
                rejoin.to_str().unwrap(),
                "--enqueue-out",
                out.to_str().unwrap(),
                "--queue-box",
                &format!("A={}", box_dir.display()),
            ],
        );
        assert_eq!(read_queue(&out).len(), expected, "{stdout}");
        if expected == 0 {
            assert!(stdout.contains("照合除外=2"), "{stdout}");
            assert!(
                table
                    .lines()
                    .filter(|l| l.starts_with("rejoin\tb"))
                    .all(|l| l.contains("dig (A,")),
                "{table}"
            );
            assert!(md.contains("dig (A,"), "{md}");
        }
    }
    let input = box_dir.join("enqueue/a.jsonl");
    let mut args = enqueue_args(&input);
    args.box_specs = vec![format!("A={}", box_dir.display())];
    assert!(run_to(args, &mut Vec::new()).is_err());
}

#[test]
fn enqueue_rejects_input_output_collisions_and_invalid_options() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("out");
    for name in [
        "master",
        "walk",
        "rejoin",
        "bridge",
        "bridge-audit",
        "exclude-queue",
        "exclude-through",
        "table",
        "markdown",
    ] {
        let mut args = enqueue_args(&out);
        match name {
            "master" => args.master = out.clone(),
            "walk" => args.walk = out.clone(),
            "rejoin" => args.rejoin = Some(out.clone()),
            "bridge" => args.bridge = Some(out.clone()),
            "bridge-audit" => args.bridge_audit = Some(out.clone()),
            "exclude-queue" => args.exclude_queue.push(out.clone()),
            "exclude-through" => args.exclude_through.push(out.clone()),
            "table" => args.table = Some(out.clone()),
            "markdown" => args.markdown = Some(out.clone()),
            _ => unreachable!(),
        }
        assert!(args.check().is_err(), "{name}");
    }
    let mut args = enqueue_args(&out);
    args.table = Some(d.path().join("same"));
    args.markdown = args.table.clone();
    assert!(args.check().is_err());
    for (flag, value) in [
        ("--tag", "bad.tag"),
        ("--depth", "21"),
        ("--opp-n", "9"),
        ("--enqueue-heavy-weight", "NaN"),
        ("--enqueue-priority-heavy", "1000001"),
    ] {
        let args = Cli::try_parse_from([
            "test",
            "--master",
            "master.tsv",
            "--walk",
            "walk.tsv",
            "--enqueue-out",
            out.to_str().unwrap(),
            flag,
            value,
        ])
        .unwrap()
        .args;
        assert!(args.check().is_err(), "{flag}");
    }
}

#[test]
fn enqueue_validates_all_lines_before_changing_outputs() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    std::fs::write(&out, "keep").unwrap();
    let rejoin = write_rejoins(&d);
    let text = std::fs::read_to_string(&rejoin).unwrap().replace("\t4c4d\t", "\t7g7f\t");
    std::fs::write(&rejoin, text).unwrap();
    let master = d.path().join("moves.tsv");
    std::fs::write(&master, format_master(&[])).unwrap();
    let walk = d.path().join("walk.tsv");
    std::fs::write(&walk, format!("{WALK_HEADER}\n")).unwrap();
    let table = d.path().join("report.tsv");
    std::fs::write(&table, "keep table").unwrap();
    let mut args = enqueue_args(&out);
    args.master = master;
    args.walk = walk;
    args.rejoin = Some(rejoin);
    args.table = Some(table.clone());
    assert!(run_to(args, &mut Vec::new()).is_err());
    assert_eq!(std::fs::read_to_string(out).unwrap(), "keep");
    assert_eq!(std::fs::read_to_string(table).unwrap(), "keep table");
}

#[test]
fn path_checks_direct_positions_and_own_playable_moves_before_root() {
    let d = TempDir::new().unwrap();
    let path = ["7g7f", "3c3d", "2g2f"];
    let walk = [walk_line("b", "own", 0.02, &path)];
    let mut own = master_row(&path[..2], path[2], "10", "single");
    own.set("role", "alt".into());
    let mut rows = vec![
        master_row(&[], path[0], "60", "tree:3"),
        master_row(&path[..1], "8c8d", "10", "single"),
        own,
    ];
    let (_, table, _) = run_report_with_rows(&d, &walk, &[], &rows);
    assert!(table.ends_with("\tok\tnot-requested\n"), "{table}");

    rows.remove(1);
    rows[1].set("role", "bench".into());
    let (_, table, _) = run_report_with_rows(&d, &walk, &[], &rows);
    assert!(table.ends_with("\tpos:2,move:3\tnot-requested\n"), "{table}");
}

#[test]
fn path_checks_the_rejoin_move_into_dig_but_not_the_reply() {
    let d = TempDir::new().unwrap();
    let rejoin = write_rejoins(&d);
    let walk = [walk_line("b", "stop", 0.02, &["7g7f", "3c3d"])];
    let mut rows = vec![
        master_row(&[], "7g7f", "60", "tree:3"),
        master_row(&["7g7f"], "8c8d", "10", "single"),
        master_row(&["7g7f", "3c3d"], "5g5f", "10", "single"),
    ];
    let options = ["--rejoin", rejoin.to_str().unwrap()];
    let (_, table, _) = run_report_with_rows(&d, &walk, &options, &rows);
    assert_eq!(table.matches("\tmove:3\tnot-requested\n").count(), 2, "{table}");

    rows[2].set("move", "2g2f".into());
    let (_, table, _) = run_report_with_rows(&d, &walk, &options, &rows);
    assert_eq!(table.matches("\tok\tnot-requested\n").count(), 2, "{table}");
    assert!(table.ends_with("\t-\tnot-requested\n"), "{table}");
}

#[test]
fn enqueue_only_counts_coverage_from_known_eligible_states() {
    let d = TempDir::new().unwrap();
    let out = d.path().join("plan.jsonl");
    let exclude = d.path().join("exclude.jsonl");
    let box_dir = d.path().join("box");
    std::fs::create_dir_all(box_dir.join("enqueue")).unwrap();
    std::fs::create_dir_all(box_dir.join("fetch")).unwrap();
    let box_spec = format!("A={}", box_dir.display());
    let walk = [walk_line("b", "own", 0.02, &["7g7f"])];
    let root = format!("sfen {} 2", key_after(&["7g7f"]));
    let queue_row =
        |id, hold| serde_json::json!({"id":id, "root":root, "own":"b", "depth":2, "hold":hold});
    let text = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        queue_row("held", true),
        queue_row("rejected", false),
        queue_row("failed", false),
        queue_row("unknown", false),
        queue_row("running", false),
        queue_row("terminal", false)
    );
    std::fs::write(box_dir.join("enqueue/a.jsonl"), &text).unwrap();
    std::fs::write(
        box_dir.join("enqueue/a.jsonl.receipt"),
        serde_json::json!({"start":0,"end":text.len(),"lines":6}).to_string(),
    )
    .unwrap();
    let progress_row = |id, hold, failed, finished| {
        serde_json::json!({
            "id":id, "own":"b", "priority":100, "hold":hold, "failed":failed,
            "finished":finished, "read":10, "requested":20
        })
    };
    let progress = box_dir.join("fetch/progress.json");
    std::fs::write(
        &progress,
        serde_json::json!({
            "trees":[progress_row("held", true, false, true), progress_row("failed", false, true, true),
                     progress_row("running", false, false, false), progress_row("terminal", false, false, true)],
            "workers":[]
        })
        .to_string(),
    )
    .unwrap();
    let rejected_start = text.find('\n').unwrap() + 1;
    let status = box_dir.join("fetch/targets.status.json");
    let mut snapshot = serde_json::json!({
        "targets":[{"id":"terminal","own":"b","state":"NOT_READY"}],
        "rejected":[{

            "start": rejected_start, "end": rejected_start + text[rejected_start..].find('\n').unwrap() + 1,
            "id":"rejected", "reason":"test"
        }]
    });
    std::fs::write(&status, snapshot.to_string()).unwrap();
    std::fs::write(&exclude, format!("{}\n", queue_row("manual", true))).unwrap();
    let (_, table, _) = run_report(
        &d,
        &walk,
        &[
            "--enqueue-out",
            out.to_str().unwrap(),
            "--box",
            &box_spec,
            "--exclude-queue",
            exclude.to_str().unwrap(),
        ],
    );
    assert_eq!(read_queue(&out).len(), 1);
    for shown in [
        "held (A, hold)",
        "rejected (A, rejected)",
        "failed (A, failed)",
        "unknown (A, unknown)",
        "running (A, 10/20)",
        "terminal (A, NOT_READY)",
        "manual (exclude-queue)",
    ] {
        assert!(table.contains(shown), "{table}");
    }

    snapshot["targets"][0]["state"] = serde_json::json!("DONE");
    std::fs::write(&status, snapshot.to_string()).unwrap();
    let (_, table, _) =
        run_report(&d, &walk, &["--enqueue-out", out.to_str().unwrap(), "--box", &box_spec]);
    assert!(read_queue(&out).is_empty());
    assert!(table.ends_with("\tok\tcovered:terminal(A,DONE)\n"), "{table}");

    std::fs::remove_file(progress).unwrap();
    std::fs::remove_file(status).unwrap();
    let text = format!("{}\n{}\n", queue_row("held", true), queue_row("waiting", false));
    std::fs::write(box_dir.join("enqueue/a.jsonl"), &text).unwrap();
    std::fs::write(
        box_dir.join("enqueue/a.jsonl.receipt"),
        serde_json::json!({"start":0,"end":text.len(),"lines":2}).to_string(),
    )
    .unwrap();
    for (arg, expected, shown) in [
        ("--box", 1, "waiting (A, unknown)"),
        ("--queue-box", 0, "covered:waiting(A,waiting)"),
    ] {
        let (_, table, _) =
            run_report(&d, &walk, &["--enqueue-out", out.to_str().unwrap(), arg, &box_spec]);
        assert_eq!(read_queue(&out).len(), expected);
        assert!(table.contains(shown), "{table}");
    }
}
