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
    let master = d.path().join("moves.tsv");
    let rows = [
        master_row(&[], "7g7f", "60", "tree:3"),
        master_row(&["7g7f", "3c3d"], "2g2f", "40", "tree:2"),
        master_row(&["7g7f", "3c3d", "2g2f"], "8c8d", "-90", "single"),
        master_row(&["2g2f"], "8c8d", "10", "single"),
    ];
    std::fs::write(&master, format_master(&rows)).unwrap();
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
    assert_eq!(stdout.trim(), "good=1 bad=1 gap=0 rejoin=0 shallow=1 weak=1");
    let rows: Vec<Vec<_>> = table.lines().skip(1).map(|l| l.split('\t').collect()).collect();
    let summary: Vec<_> = rows.iter().map(|r| (r[0], r[1], r[4], r[5], r[6], r[7])).collect();
    assert_eq!(
        summary,
        [
            ("good", "b", "0.03", "+60", "tree:3", "届く"),
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
    let exit = ["7g7f", "3c3d", "2g2f"];
    let walk = [walk_line("b", "max", 0.02, &exit)];
    let key = key_after(&exit);
    let bridge = d.path().join("bridge.tsv");
    std::fs::write(
        &bridge,
        format!(
            "key\tply\tside\tmain\tplain\tbridged\tdrop\tbest\tbest_value\tplain_black\tbridged_black\tweight\n\
             {key}\t4\tw\t8c8d\t0\t-10\t10\t\t\t0\t10\t\n"
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
    assert!(table.contains("gap\tb\tw\t4\t0.02\t+10\t\t届かない"), "{table}");
}

#[test]
fn bridge_targets_and_weights_follow_each_walk_side() {
    let d = TempDir::new().unwrap();
    let path = ["2g2f"];
    let key = key_after(&path);
    let bridge = d.path().join("bridge.tsv");
    let audit = d.path().join("audit.tsv");
    std::fs::write(&audit, "key\tgap\n").unwrap();
    for (value, expected_side, weight) in [(82, "w", "0.003"), (41, "b", "0.0095")] {
        std::fs::write(
            &bridge,
            format!(
                "key\tply\tside\tmain\tplain_black\tbridged_black\n{key}\t2\tw\t8c8d\t0\t{value}\n"
            ),
        )
        .unwrap();
        let walk = [
            walk_line("b", "out", 0.0095, &path),
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
        assert_eq!(gaps.len(), 1, "{table}");
        assert_eq!((gaps[0][1], gaps[0][2], gaps[0][4]), (expected_side, "w", weight));
        assert!(
            md.contains(if expected_side == "b" {
                "RAMU が先手 / 後手 2 手目"
            } else {
                "RAMU が後手 / 後手 2 手目"
            }),
            "{md}"
        );
    }
    let walk = [
        walk_line("b", "out", 0.0006, &path),
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
