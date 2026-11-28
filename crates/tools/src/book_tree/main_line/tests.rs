use super::super::master_table::{format_master, test_row};
use super::*;
use clap::Parser;
use tempfile::TempDir;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: MainLineArgs,
}

/// 初期局面から `moves` を辿った局面に `role`・`evals` の行を置く。
fn row(moves: &[&str], mv: &str, role: &str, evals: &str) -> super::super::master_table::Row {
    let mut key = HIRATE_KEY.to_owned();
    for m in moves {
        key = child(&key, m).unwrap();
    }
    test_row(&[
        ("key", key),
        ("move", mv.into()),
        ("role", role.into()),
        ("evals", evals.into()),
    ])
}

fn run_with(dir: &TempDir, rows: &[super::super::master_table::Row], extra: &[&str]) -> String {
    let master = dir.path().join("moves.tsv");
    std::fs::write(&master, format_master(rows)).unwrap();
    let table = dir.path().join("line.tsv");
    let out = dir.path().join("line.jsonl");
    let mut argv = vec![
        "test".to_owned(),
        "--master".into(),
        master.display().to_string(),
        "--table".into(),
        table.display().to_string(),
        "--enqueue-out".into(),
        out.display().to_string(),
    ];
    argv.extend(extra.iter().map(|s| s.to_string()));
    let mut stdout = Vec::new();
    run_to(Cli::try_parse_from(argv).unwrap().args, &mut stdout).unwrap();
    String::from_utf8(stdout).unwrap()
}

fn lines(dir: &TempDir) -> Vec<Line> {
    std::fs::read_to_string(dir.path().join("line.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn missing_position_emits_bw_tree_and_black_view_values() {
    let d = TempDir::new().unwrap();
    let rows = [
        row(&[], "7g7f", "main", "30"),
        row(&[], "2g2f", "alt", "20"),
        row(&["7g7f"], "3c3d", "main", "-20"),
    ];
    let stdout = run_with(&d, &rows, &["--priority", "7", "--depth", "4"]);
    assert!(stdout.starts_with("2 手で切れる。理由: missing"), "{stdout}");
    assert!(stdout.contains("次は先手番"), "{stdout}");
    let table = std::fs::read_to_string(d.path().join("line.tsv")).unwrap();
    let cells: Vec<Vec<_>> = table.lines().map(|l| l.split('\t').collect()).collect();
    assert_eq!(cells[0], ["ply", "key", "move", "value_black", "reason"]);
    assert_eq!((cells[1][2], cells[1][3]), ("7g7f", "30"));
    assert_eq!((cells[2][2], cells[2][3]), ("3c3d", "20"));
    assert_eq!((cells[3][0], cells[3][4]), ("2", "missing"));
    let queued = lines(&d);
    assert_eq!(queued.len(), 1);
    let line = &queued[0];
    let key = child(&child(HIRATE_KEY, "7g7f").unwrap(), "3c3d").unwrap();
    assert_eq!(line.root, format!("sfen {key} 3"));
    assert_eq!((line.own.as_str(), line.depth, line.priority), ("bw", 4, 7));
    assert_eq!(line.memo.as_deref(), Some("main-line reason=missing moves=7g7f 3c3d"));
    assert_eq!(line.id, queue_id("mainline", 3, &key, "bw"));
}

#[test]
fn flipped_only_and_no_main_stop_with_tree_lines() {
    let d = TempDir::new().unwrap();
    let after = child(HIRATE_KEY, "7g7f").unwrap();
    let flipped =
        crate::book_db::strip_ply(&rshogi_book::flipped_key(&format!("{after} 1")).unwrap())
            .to_owned();
    let mirror = test_row(&[
        ("key", flipped),
        ("move", "7g7f".into()),
        ("role", "main".into()),
        ("evals", String::new()),
    ]);
    let stdout = run_with(&d, &[row(&[], "7g7f", "main", ""), mirror], &[]);
    assert!(stdout.starts_with("1 手で切れる。理由: flipped-only"), "{stdout}");
    assert_eq!(lines(&d)[0].memo.as_deref(), Some("main-line reason=flipped-only moves=7g7f"));

    let d = TempDir::new().unwrap();
    let rows = [
        row(&[], "7g7f", "main", ""),
        row(&["7g7f"], "3c3d", "alt", ""),
        row(&["7g7f"], "8c8d", "record", ""),
    ];
    let stdout = run_with(&d, &rows, &[]);
    assert!(
        stdout.contains("理由: no-main (局面はあるが main が無い (role: alt,record))"),
        "{stdout}"
    );
    assert_eq!(lines(&d).len(), 1);
}

#[test]
fn cycle_and_max_ply_write_no_tree_line() {
    let d = TempDir::new().unwrap();
    let path = ["5i5h", "5a5b", "5h5i", "5b5a"];
    let rows: Vec<_> = (0..4).map(|i| row(&path[..i], path[i], "main", "")).collect();
    let stdout = run_with(&d, &rows, &[]);
    assert!(stdout.starts_with("4 手で切れる。理由: cycle"), "{stdout}");
    assert!(lines(&d).is_empty());

    let d = TempDir::new().unwrap();
    let stdout = run_with(&d, &rows, &["--max-ply", "2"]);
    assert!(stdout.starts_with("2 手で切れる。理由: max-ply"), "{stdout}");
    assert!(stdout.contains("moves=5i5h 5a5b"), "{stdout}");
    assert!(lines(&d).is_empty());
}

#[test]
fn cycle_stops_at_a_position_already_visited_in_start_moves() {
    let path = ["7g7f", "3c3d", "5i5h", "5a5b", "5h5i", "5b5a"];
    for (path, start_ply) in [(&path[2..], 1), (path.as_slice(), 3)] {
        let d = TempDir::new().unwrap();
        let rows: Vec<_> =
            (start_ply..path.len()).map(|i| row(&path[..i], path[i], "main", "")).collect();
        let prefix = path[..start_ply].join(" ");
        let stdout = run_with(&d, &rows, &["--start-moves", &prefix]);
        assert!(
            stdout.starts_with(&format!("{} 手で切れる。理由: cycle", path.len())),
            "{stdout}"
        );
        assert!(stdout.contains(&format!("moves={}\n", path.join(" "))), "{stdout}");
        assert!(lines(&d).is_empty());
    }
}

#[test]
fn outputs_must_differ_from_inputs() {
    let d = TempDir::new().unwrap();
    let master = d.path().join("moves.tsv");
    std::fs::write(&master, format_master(&[row(&[], "7g7f", "main", "")])).unwrap();
    let m = master.display().to_string();
    for extra in [["--table", m.as_str()], ["--enqueue-out", m.as_str()]] {
        let argv = ["test", "--master", m.as_str()].into_iter().chain(extra);
        let args = Cli::try_parse_from(argv).unwrap().args;
        assert!(run_to(args, &mut Vec::new()).is_err());
    }
}

#[test]
fn stop_at_max_ply_keeps_readable_reason_and_two_mains_fail() {
    let d = TempDir::new().unwrap();
    let stdout = run_with(&d, &[row(&[], "7g7f", "main", "")], &["--max-ply", "1"]);
    assert!(stdout.starts_with("1 手で切れる。理由: missing"), "{stdout}");
    assert_eq!(lines(&d).len(), 1);

    let d = TempDir::new().unwrap();
    let master = d.path().join("moves.tsv");
    std::fs::write(
        &master,
        format_master(&[row(&[], "7g7f", "main", ""), row(&[], "2g2f", "main", "")]),
    )
    .unwrap();
    let args = Cli::try_parse_from(["test", "--master", master.to_str().unwrap()])
        .unwrap()
        .args;
    let error = run_to(args, &mut Vec::new()).unwrap_err().to_string();
    assert!(error.contains("main が 2 行"), "{error}");
}

#[test]
fn starts_after_legal_prefix_and_counts_it_in_table_and_limit() {
    let d = TempDir::new().unwrap();
    let rows = [
        row(&["7g7f"], "3c3d", "main", "-20"),
        row(&["7g7f", "3c3d"], "8h2b+", "main", "60"),
    ];
    let stdout = run_with(&d, &rows, &["--start-moves", "7g7f", "--max-ply", "2"]);
    assert!(stdout.starts_with("2 手で切れる。理由: max-ply"), "{stdout}");
    assert!(stdout.contains("moves=7g7f 3c3d\n"), "{stdout}");
    let table = std::fs::read_to_string(d.path().join("line.tsv")).unwrap();
    let after_prefix = child(HIRATE_KEY, "7g7f").unwrap();
    let end = child(&after_prefix, "3c3d").unwrap();
    assert_eq!(
        table,
        format!(
            "ply\tkey\tmove\tvalue_black\treason\n0\t{HIRATE_KEY}\t7g7f\t\t\n1\t{after_prefix}\t3c3d\t20\t\n2\t{end}\t\t\tmax-ply\n"
        )
    );
}

#[test]
fn rejects_invalid_prefix_moves_with_their_ply() {
    let d = TempDir::new().unwrap();
    let master = d.path().join("moves.tsv");
    std::fs::write(&master, format_master(&[row(&[], "7g7f", "main", "")])).unwrap();
    let master = master.to_str().unwrap();
    for mv in ["bad", "3c3dextra", "7f7e", "pass"] {
        let prefix = format!("7g7f {mv}");
        let args = Cli::parse_from(["test", "--master", master, "--start-moves", &prefix]).args;
        let err = run_to(args, &mut Vec::new()).unwrap_err().to_string();
        assert!(err.contains("--start-moves の 2 手目") && err.contains(mv), "{err}");
    }
    let args = Cli::parse_from([
        "test",
        "--master",
        master,
        "--start-moves",
        "7g7f 3c3d",
        "--max-ply",
        "1",
    ])
    .args;
    let err = run_to(args, &mut Vec::new()).unwrap_err().to_string();
    assert!(err.contains("--max-ply 1"), "{err}");
}

#[test]
fn exports_kif_with_direct_book_values_and_promotion() {
    use crate::book_db::BOOK_HEADER;
    use crate::kifu_publish::kif_convert::kif_to_csa;

    let d = TempDir::new().unwrap();
    let path = ["7g7f", "3c3d", "8h2b+", "3a2b", "2g2f"];
    let rows: Vec<_> = (2..path.len()).map(|i| row(&path[..i], path[i], "main", "9")).collect();
    let keys: Vec<_> = path
        .iter()
        .scan(HIRATE_KEY.to_owned(), |key, mv| {
            let next = child(key, mv).unwrap();
            Some(std::mem::replace(key, next))
        })
        .collect();
    let values = d.path().join("values.db");
    let flipped = rshogi_book::flipped_key(&format!("{} 3", keys[2])).unwrap();
    let flipped_move = rshogi_book::flip_usi_move(path[2]).unwrap();
    std::fs::write(
        &values,
        format!(
            "{BOOK_HEADER}\nsfen {} 99\n7g7f none 101 1 2\nsfen {} 88\n3c3d none -2147483648 1 2\nsfen {flipped}\n{flipped_move} none 999 1 2\nsfen {} 4\n8c8d none invalid 1 1\nsfen {} 5\n2g2f none 0 1 2\n",
            keys[0], keys[1], keys[3], keys[4],
        ),
    )
    .unwrap();
    let kif = d.path().join("line.kif");
    let args = ["--start-moves", "7g7f 3c3d", "--kif", kif.to_str().unwrap()];
    let mut with_values = args.to_vec();
    with_values.extend(["--values-db", values.to_str().unwrap()]);
    let stdout = run_with(&d, &rows, &with_values);
    assert!(stdout.contains(&format!("moves={}\n", path.join(" "))), "{stdout}");
    let text = std::fs::read_to_string(&kif).unwrap();
    assert_eq!(
        text,
        concat!(
            "手合割：平手\n先手：\n後手：\n手数----指手---------消費時間--\n",
            "   1 ７六歩(77)\n**評価値=101\n",
            "   2 ３四歩(33)\n**評価値=2147483648\n*ここから定跡の main\n",
            "   3 ２二角成(88)\n   4 同　銀(31)\n   5 ２六歩(27)\n**評価値=0\n",
            "   6 中断\n\nまで5手で中断\n",
        )
    );
    // CSA 変換に必要な対局情報を補い、中断は変換器が扱える終局行なしの形式にする。
    let for_csa = format!(
        "開始日時：2026/01/01 12:00:00\n{}",
        text.replace("先手：\n後手：\n", "先手：先手\n後手：後手\n")
            .strip_suffix("   6 中断\n\nまで5手で中断\n")
            .unwrap()
    );
    let csa = kif_to_csa(&for_csa).unwrap();
    let moves: Vec<_> = csa
        .lines()
        .filter(|line| line.starts_with(['+', '-']) && line.len() == 7)
        .collect();
    assert_eq!(moves, ["+7776FU", "-3334FU", "+8822UM", "-3122GI", "+2726FU"]);

    run_with(&d, &rows, &args);
    let without_values = std::fs::read_to_string(&kif).unwrap();
    assert!(!without_values.contains("**評価値"));
}

#[test]
fn exports_kif_with_drop_and_unpromoted_move() {
    let d = TempDir::new().unwrap();
    let prefix = ["7g7f", "3c3d", "8h2b", "3a2b"];
    let rows = [row(&prefix, "B*4e", "main", "")];
    let kif = d.path().join("line.kif");
    run_with(
        &d,
        &rows,
        &[
            "--start-moves",
            &prefix.join(" "),
            "--kif",
            kif.to_str().unwrap(),
        ],
    );
    let text = std::fs::read_to_string(&kif).unwrap();
    assert!(text.lines().any(|line| line == "   3 ２二角(88)"), "{text}");
    assert!(text.lines().any(|line| line == "   5 ４五角打"), "{text}");
}

#[test]
fn kif_rejects_missing_or_invalid_values_on_matching_rows() {
    let d = TempDir::new().unwrap();
    let values = d.path().join("values.db");
    let (key, steps) = start_path("7g7f", 400).unwrap();
    let walked = Walked {
        steps,
        start_ply: 1,
        key,
        stop: Stop::Missing,
    };
    for row in ["7g7f none", "7g7f none invalid", "7g7f none 2147483648"] {
        std::fs::write(&values, format!("sfen {HIRATE_KEY} 1\n2g2f none\n{row}\n")).unwrap();
        let err = render_kif(&walked, Some(&values)).unwrap_err().to_string();
        assert!(err.contains(&format!("{} の 3 行目", values.display())), "{err}");
        assert!(err.contains("7g7f の評価値欄"), "{err}");
    }
}

#[test]
fn read_values_stops_after_the_last_requested_value() {
    let d = TempDir::new().unwrap();
    let values = d.path().join("values.db");
    let (_, steps) = start_path("7g7f 3c3d", 400).unwrap();
    let mut db = format!(
        "sfen {HIRATE_KEY} 1\n7g7f none 101\n7g7f none 999\nsfen {} 2\n3c3d none 202\n",
        steps[1].key
    )
    .into_bytes();
    db.extend_from_slice(b"\xff\n");
    std::fs::write(&values, db).unwrap();
    let loaded = read_values(&values, &steps).unwrap();
    assert_eq!(loaded[HIRATE_KEY]["7g7f"], Some(101));
    assert_eq!(loaded[steps[1].key.as_str()]["3c3d"], Some(202));
}

#[test]
fn values_db_requires_kif() {
    let Err(err) = Cli::try_parse_from(["test", "--master", "moves.tsv", "--values-db", "a.db"])
    else {
        panic!("--kif が必要です");
    };
    assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    assert!(err.to_string().contains("--kif"), "{err}");
}

#[test]
fn kif_and_values_db_participate_in_output_path_checks() {
    let d = TempDir::new().unwrap();
    let master = d.path().join("moves.tsv");
    let schema = d.path().join("schema");
    std::fs::create_dir(&schema).unwrap();
    let schema_file = schema.join("sources.tsv");
    let other = d.path().join("other");
    let kif = d.path().join("line.kif");
    let master = master.to_str().unwrap();
    let schema = schema.to_str().unwrap();
    let schema_file = schema_file.to_str().unwrap();
    let other = other.to_str().unwrap();
    let kif = kif.to_str().unwrap();
    for (options, conflicting) in [
        (vec!["--kif", master], "--master"),
        (vec!["--kif", schema_file], "--schema"),
        (vec!["--kif", other, "--table", other], "--table"),
        (vec!["--kif", other, "--enqueue-out", other], "--enqueue-out"),
        (vec!["--values-db", other, "--kif", other], "--values-db"),
        (vec!["--values-db", other, "--table", other, "--kif", kif], "--values-db"),
        (vec!["--values-db", other, "--enqueue-out", other, "--kif", kif], "--values-db"),
    ] {
        let argv = ["test", "--master", master, "--schema", schema].into_iter().chain(options);
        let err = Cli::parse_from(argv).args.check().unwrap_err();
        assert!(format!("{err:#}").contains(&format!("{conflicting} と同じ")), "{err:#}");
    }
}
