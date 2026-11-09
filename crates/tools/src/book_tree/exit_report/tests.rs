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
        walk_line("b", "rare", 0.03, &["7g7f", "4c4d"]),
        walk_line("b", "own", 0.05, &["7g7f", "8c8d"]),
        // 後手の最後の手は 8c8d (single、後手から +10 は先手から -10)。値の出所が弱い。
        walk_line("w", "max", 0.01, &["2g2f", "8c8d", "7g7f"]),
        walk_line("b", "out", 0.001, &["7g7f", "4c4d", "2g2f"]),
        walk_line("b", "stop", 0.5, &["7g7f"]),
    ];
    let (stdout, table, md) = run_report(&d, &walk, &[]);
    assert_eq!(stdout.trim(), "good=1 bad=1 gap=0 rejoin=0 shallow=1 weak=1");
    let rows: Vec<Vec<_>> = table.lines().skip(1).map(|l| l.split('\t').collect()).collect();
    let summary: Vec<_> = rows.iter().map(|r| (r[0], r[1], r[3], r[4], r[5], r[6])).collect();
    assert_eq!(
        summary,
        [
            ("good", "b", "0.03", "+60", "tree:3", "届く"),
            ("bad", "b", "0.02", "+40", "tree:2", "届かない"),
            ("shallow", "b", "0.05", "+60", "tree:3", "届く"),
            ("weak", "w", "0.01", "-10", "single", "届く"),
        ]
    );
    assert!(
        md.contains(
            "| 4 | 0.020 | +40 | tree:2 | 届かない | なし | ▲７六歩 △３四歩 ▲２六歩 △８四歩 |"
        ),
        "{md}"
    );
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
             w\t{light_key}\t2\t8c8d\tmain\t0\t\t3c3d\t\t\t\t\t0\t120\t0.1\t10\t2\n\
             w\t{light_key}\t2\t8c8d\tmain\t0\t\t4c4d\t\t\t\t\t0\t30\t0.1\t10\t2\n\
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
    assert!(table.contains("gap\tb\t3\t0.02\t+10\t\t届かない\tなし\t"), "{table}");
    assert!(
        table.contains("main=2g2f;plain_black=40;best=6g6f;best_value_black=60"),
        "{table}"
    );
    assert!(table.contains("rejoin\tw\t2\t0.02\t+120\t\t届かない"), "{table}");
    assert!(
        md.contains(
            "| 先手 | 3 | 0.020 | ▲２六歩 | +40 → +10 | ▲６六歩 +60 | なし | ▲７六歩 △３四歩 |"
        ),
        "{md}"
    );
    assert!(md.contains("| 後手 | 2 | 0.0200 | 8c8d (0) | 3c3d → +120 |"), "{md}");
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
